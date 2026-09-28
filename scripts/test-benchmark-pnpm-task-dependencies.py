#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import runpy
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / 'scripts' / 'benchmark-pnpm-task-dependencies'
NAMESPACE = runpy.run_path(str(SCRIPT), run_name='pnpm_dependency_benchmark_test')


class PnpmTaskDependencyBenchmarkTests(unittest.TestCase):
    def plan(
        self,
        *,
        fanout: int = 8,
        import_method: str = 'clone',
        repetitions: int = 3,
        deadline_seconds: int = 600,
        probe_script: str | None = 'test',
    ) -> object:
        return NAMESPACE['Plan'](
            source=Path('/source'),
            scratch_root=Path('/scratch'),
            pnpm=Path('/usr/bin/pnpm'),
            fanout=fanout,
            import_method=import_method,
            repetitions=repetitions,
            deadline_seconds=deadline_seconds,
            probe_script=probe_script,
        )

    def test_plan_is_closed_to_trusted_1_8_32_matrix(self) -> None:
        validate = NAMESPACE['validate_plan']
        for fanout in (1, 8, 32):
            plan = self.plan(fanout=fanout)
            validate(plan)
            document = plan.to_json()
            self.assertEqual(document['trust_class'], 'ultra_trusted_only')
            self.assertTrue(document['probe_script_digest'].startswith('sha256:'))
            self.assertEqual(len(document['probe_script_digest']), 71)
            self.assertTrue(document['workspace_policy']['unique_git_worktree_per_task'])
            self.assertTrue(document['workspace_policy']['unique_node_modules_root_per_task'])
            self.assertEqual(
                document['workspace_policy']['task_local_pnpm_virtual_store'],
                'node_modules/.pnpm',
            )
            self.assertEqual(
                document['workspace_policy']['virtual_store_type'],
                'project_forced_by_closed_environment',
            )
            self.assertTrue(
                document['workspace_policy']['repository_execution_requires_task_private_dependency_bytes']
            )
            self.assertEqual(
                document['workspace_policy']['resident_store_mutation'],
                'forbidden_by_pnpm_frozen_store',
            )
            self.assertFalse(document['workspace_policy']['shared_task_workspace'])
            self.assertFalse(document['workspace_policy']['directory_presence_grants_task_authority'])
            self.assertEqual(document['network_policy']['dependency_install'], 'pnpm_offline')
            self.assertEqual(
                document['network_policy']['first_read_only_command'],
                'local_installed_dependency_graph',
            )
            self.assertEqual(
                document['network_policy']['repository_probe'],
                'inherits_trusted_host_network',
            )
            self.assertEqual(
                self.plan(probe_script=None).to_json()['network_policy']['repository_probe'],
                'absent',
            )
            self.assertEqual(
                document['storage_accounting']['filesystem_available_space'],
                'statvfs_f_bavail',
            )
            self.assertEqual(
                document['storage_accounting']['node_modules_allocated_blocks'],
                'sum_st_blocks_times_512',
            )
            self.assertFalse(
                document['storage_accounting']['reflink_exclusive_ownership_inferred']
            )
            self.assertNotIn('/source', json.dumps(document))
            self.assertNotIn('/scratch', json.dumps(document))

    def test_plan_refuses_unreviewed_widths_methods_deadlines_and_probe_names(self) -> None:
        validate = NAMESPACE['validate_plan']
        BenchmarkError = NAMESPACE['BenchmarkError']
        for plan in (
            self.plan(fanout=4),
            self.plan(import_method='copy'),
            self.plan(repetitions=0),
            self.plan(repetitions=101),
            self.plan(deadline_seconds=0),
            self.plan(deadline_seconds=3601),
            self.plan(probe_script='test all'),
            self.plan(import_method='hardlink', probe_script='test'),
        ):
            with self.assertRaises(BenchmarkError):
                validate(plan)

    def test_ready_capacity_observation_uses_direct_filesystem_calls(self) -> None:
        observe = NAMESPACE['filesystem_capacity_observation']
        with tempfile.TemporaryDirectory() as root_text:
            root = Path(root_text)
            observed = root.stat()
            filesystem = os.statvfs(root)
            fragment_size = filesystem.f_frsize or filesystem.f_bsize
            baseline = {
                'mount_id': 'baseline',
                'device_major': os.major(observed.st_dev),
                'device_minor': os.minor(observed.st_dev),
                'findmnt_device': 'baseline-device',
                'filesystem_type': 'xfs',
                'fragment_size_bytes': fragment_size,
                'available_bytes': 0,
                'available_inodes': 0,
            }
            with mock.patch.object(
                observe.__globals__['subprocess'],
                'run',
                side_effect=AssertionError('capacity observation spawned a child'),
            ):
                result = observe(root, baseline)
        self.assertEqual(result['mount_id'], 'baseline')
        self.assertEqual(result['filesystem_type'], 'xfs')
        self.assertGreater(result['available_bytes'], 0)

    def test_storage_deltas_preserve_negative_noise_and_cleanup_debt(self) -> None:
        delta = NAMESPACE['consumption_delta']
        recovered = NAMESPACE['recovered_capacity']
        self.assertEqual(delta(100, 80), 20)
        self.assertEqual(delta(100, 120), -20)
        self.assertEqual(recovered(80, 100), 20)
        self.assertEqual(recovered(100, 80), -20)

    def test_package_json_read_is_bounded(self) -> None:
        package_identity = NAMESPACE['package_identity']
        BenchmarkError = NAMESPACE['BenchmarkError']
        with tempfile.TemporaryDirectory() as root_text:
            root = Path(root_text)
            (root / 'package.json').write_text('{}', encoding='utf-8')
            (root / 'pnpm-lock.yaml').write_text('lockfileVersion: 9\n', encoding='utf-8')
            with mock.patch.dict(
                package_identity.__globals__,
                {'MAX_PACKAGE_JSON_BYTES': 1},
            ):
                with self.assertRaises(BenchmarkError):
                    package_identity(root)

    def test_public_failure_message_keeps_os_paths_out_of_receipts(self) -> None:
        message = NAMESPACE['public_failure_message'](
            OSError(2, 'missing', '/private/project/secret')
        )
        self.assertEqual(message, 'benchmark_operation_failed')
        benchmark_error = NAMESPACE['BenchmarkError']('bounded_failure')
        self.assertEqual(
            NAMESPACE['public_failure_message'](benchmark_error),
            'bounded_failure',
        )

    def test_install_command_closes_network_lockfile_scripts_store_and_import_method(self) -> None:
        command = NAMESPACE['install_command'](
            Path('/pnpm'), Path('/store'), 'clone'
        )
        self.assertEqual(command[0:2], ['/pnpm', 'install'])
        self.assertIn('--frozen-store', command)
        self.assertIn('--offline', command)
        self.assertIn('--frozen-lockfile', command)
        self.assertIn('--ignore-scripts', command)
        self.assertNotIn('--virtual-store-type=project', command)
        self.assertIn('--virtual-store-dir=node_modules/.pnpm', command)
        self.assertIn('--store-dir=/store', command)
        self.assertIn('--package-import-method=clone', command)

    def test_closed_environment_forces_project_virtual_store_type(self) -> None:
        environment = NAMESPACE['closed_environment'](Path('/opt/node/bin/node'))
        self.assertEqual(
            environment['PNPM_CONFIG_VIRTUAL_STORE_TYPE'],
            'project',
        )
        self.assertEqual(environment['NPM_CONFIG_USERCONFIG'], '/dev/null')

    def test_pnpm_virtual_store_type_preflight_requires_project(self) -> None:
        helper = NAMESPACE['pnpm_virtual_store_type']
        BenchmarkError = NAMESPACE['BenchmarkError']
        environment = {'PNPM_CONFIG_VIRTUAL_STORE_TYPE': 'project'}
        with mock.patch.dict(
            helper.__globals__,
            {'run_text': mock.Mock(return_value='project')},
        ):
            self.assertEqual(
                helper(Path('/pnpm'), Path('/source'), environment),
                'project',
            )
        with mock.patch.dict(
            helper.__globals__,
            {'run_text': mock.Mock(return_value='global')},
        ):
            with self.assertRaises(BenchmarkError):
                helper(Path('/pnpm'), Path('/source'), environment)

    def test_first_read_only_command_uses_local_dependency_state(self) -> None:
        command = NAMESPACE['first_read_only_command'](Path('/pnpm'))
        self.assertEqual(command, ['/pnpm', 'list', '--depth=0'])
        self.assertNotIn('--offline', command)

    def test_physical_mechanism_classifier_keeps_clone_hardlink_and_copy_distinct(self) -> None:
        classify = NAMESPACE['classify_mechanism']
        self.assertEqual(classify(10, 4, 10, 0), 'hardlink_observed')
        self.assertEqual(classify(10, 0, 10, 10), 'reflink_observed')
        self.assertEqual(
            classify(10, 0, 10, 3),
            'mixed_private_copy_and_reflink_observed',
        )
        self.assertEqual(classify(10, 0, 10, 0), 'copy_observed')
        self.assertEqual(classify(10, 0, 9, 9), 'physical_mechanism_unproven')
        with self.assertRaises(NAMESPACE['BenchmarkError']):
            classify(0, 0, 0, 0)

    def test_physical_sample_targets_virtual_store_package_payloads(self) -> None:
        sample = NAMESPACE['sample_physical_mechanism']
        with tempfile.TemporaryDirectory() as root_text:
            node_modules = Path(root_text) / 'node_modules'
            payload = (
                node_modules
                / '.pnpm'
                / 'pkg@1.0.0'
                / 'node_modules'
                / 'pkg'
                / 'index.js'
            )
            payload.parent.mkdir(parents=True)
            payload.write_bytes(b'payload')
            (node_modules / '.modules.yaml').write_text('layoutVersion: 5\n', encoding='utf-8')
            (node_modules / 'top-level-metadata').write_bytes(b'metadata')
            empty_payload = payload.with_name('empty.js')
            empty_payload.write_bytes(b'')
            with mock.patch.dict(
                sample.__globals__,
                {'fiemap_has_shared_extent': lambda path: path == payload},
            ):
                evidence = sample(node_modules)

        self.assertEqual(
            evidence['sample_scope'],
            'pnpm_virtual_store_nonempty_package_payload_regular_files',
        )
        self.assertEqual(evidence['sampled_regular_files'], 1)
        self.assertEqual(evidence['fiemap_observed_files'], 1)
        self.assertEqual(evidence['shared_extent_files'], 1)
        self.assertEqual(evidence['mechanism'], 'reflink_observed')

    def test_explicit_import_methods_require_per_task_physical_proof(self) -> None:
        validate = NAMESPACE['validate_mechanisms']
        BenchmarkError = NAMESPACE['BenchmarkError']

        def evidence(mechanism: str, hardlinks: int = 0) -> dict[str, object]:
            return {
                'usage': {
                    'regular_file_count': 10,
                    'multiply_linked_regular_file_count': hardlinks,
                },
                'sample': {
                    'mechanism': mechanism,
                },
            }

        reflink = [evidence('reflink_observed') for _ in range(8)]
        hardlink = [evidence('hardlink_observed', hardlinks=10) for _ in range(8)]
        copy = [evidence('copy_observed') for _ in range(8)]
        self.assertEqual(validate('clone', reflink), {'reflink_observed'})
        self.assertEqual(validate('hardlink', hardlink), {'hardlink_observed'})
        self.assertEqual(validate('auto', copy), {'copy_observed'})
        with self.assertRaises(BenchmarkError):
            validate('clone', reflink[:-1] + hardlink[:1])
        with self.assertRaises(BenchmarkError):
            validate('hardlink', hardlink[:-1] + copy[:1])

        hidden_hardlink = evidence('reflink_observed', hardlinks=1)
        self.assertEqual(
            validate('auto', reflink[:-1] + [hidden_hardlink]),
            {'reflink_observed', 'hardlink_observed'},
        )
        with self.assertRaises(BenchmarkError):
            validate('clone', reflink[:-1] + [hidden_hardlink])

    def test_ready_boundary_proves_git_before_first_command(self) -> None:
        measure_once = NAMESPACE['measure_once']
        events: list[str] = []
        with tempfile.TemporaryDirectory() as source_text, tempfile.TemporaryDirectory() as scratch_text:
            source = Path(source_text)
            scratch = Path(scratch_text)
            store = scratch / 'store'
            store.mkdir()
            plan = NAMESPACE['Plan'](
                source=source,
                scratch_root=scratch,
                pnpm=Path('/pnpm'),
                fanout=1,
                import_method='clone',
                repetitions=1,
                deadline_seconds=60,
                probe_script=None,
            )
            identity = {'commit': '1' * 40, 'tree': '2' * 40}
            pnpm = {'store': store, 'environment': {}}
            filesystem = {
                'mount_id': '1',
                'device_major': 1,
                'device_minor': 2,
                'findmnt_device': '1:2',
                'filesystem_type': 'xfs',
                'fragment_size_bytes': 4096,
                'available_bytes': 1_000_000,
                'available_inodes': 10_000,
            }

            def observe(_path: Path) -> dict[str, object]:
                events.append('filesystem')
                return dict(filesystem)

            def capacity(_path: Path, _baseline: dict[str, object]) -> dict[str, object]:
                events.append('capacity')
                return dict(filesystem)

            def add(_source: Path, target: Path, _commit: str) -> None:
                events.append('worktree')
                target.mkdir()
                (target / 'node_modules').mkdir()

            def cohort(commands: object, **_kwargs: object) -> dict[str, object]:
                command_list = list(commands)
                action = command_list[0][1][1]
                events.append(action)
                return {
                    'elapsed_seconds': 0.01,
                    'process_count': 1,
                    'maximum_simultaneous_observed': 1,
                    'failure_count': 0,
                    'deadline_exceeded': False,
                    'surviving_process_groups_detected': 0,
                }

            def prove(_task: Path, _commit: str, _tree: str) -> None:
                events.append('git-proof')

            def physical(_node_modules: Path) -> dict[str, object]:
                events.append('physical')
                return {
                    'sampled_regular_files': 1,
                    'hardlinked_files': 0,
                    'fiemap_observed_files': 1,
                    'shared_extent_files': 1,
                    'mechanism': 'reflink_observed',
                }

            def remove(_source: Path, target: Path) -> None:
                events.append('remove')
                (target / 'node_modules').rmdir()
                target.rmdir()

            with mock.patch.dict(
                measure_once.__globals__,
                {
                    'filesystem_observation': observe,
                    'filesystem_capacity_observation': capacity,
                    'add_worktree': add,
                    'run_cohort': cohort,
                    'prove_task_git': prove,
                    'tree_usage': lambda _root: {
                        'logical_bytes': 1,
                        'allocated_bytes': 4096,
                        'inode_count': 1,
                        'regular_file_count': 1,
                        'multiply_linked_regular_file_count': 0,
                    },
                    'sample_physical_mechanism': physical,
                    'remove_worktree': remove,
                    'rusage_snapshot': lambda: (0.0, 0.0),
                },
            ):
                sample = measure_once(plan, identity, pnpm, 1)

        self.assertEqual(sample['result'], 'succeeded')
        self.assertLess(events.index('git-proof'), events.index('capacity'))
        self.assertLess(events.index('capacity'), events.index('list'))
        self.assertLess(events.index('list'), events.index('physical'))
        self.assertIsNotNone(sample['readiness_verification_seconds'])
        self.assertIsNotNone(sample['ready_storage_observation_seconds'])
        self.assertLessEqual(
            sample['workspace_ready_seconds'],
            sample['task_known_to_first_command_start_seconds'],
        )
        self.assertIn('filesystem_available_space_delta_at_ready_bytes', sample)
        self.assertIn('filesystem_available_space_delta_during_cleanup_bytes', sample)
        self.assertEqual(sample['node_modules_st_blocks_bytes'], 4096)
        self.assertNotIn('physical_byte_delta_at_ready', sample)

    def test_partial_worktree_registration_attempt_is_cleanup_owned(self) -> None:
        measure_once = NAMESPACE['measure_once']
        events: list[str] = []
        with tempfile.TemporaryDirectory() as source_text, tempfile.TemporaryDirectory() as scratch_text:
            source = Path(source_text)
            scratch = Path(scratch_text)
            store = scratch / 'store'
            store.mkdir()
            plan = NAMESPACE['Plan'](
                source=source,
                scratch_root=scratch,
                pnpm=Path('/pnpm'),
                fanout=1,
                import_method='clone',
                repetitions=1,
                deadline_seconds=60,
                probe_script=None,
            )
            identity = {'commit': '1' * 40, 'tree': '2' * 40}
            pnpm = {'store': store, 'environment': {}}
            filesystem = {
                'mount_id': '1',
                'device_major': 1,
                'device_minor': 2,
                'findmnt_device': '1:2',
                'filesystem_type': 'xfs',
                'fragment_size_bytes': 4096,
                'available_bytes': 1_000_000,
                'available_inodes': 10_000,
            }

            def add(_source: Path, target: Path, _commit: str) -> None:
                events.append('add')
                target.mkdir()
                raise NAMESPACE['BenchmarkError']('partial_registration')

            def remove(_source: Path, target: Path) -> None:
                events.append('remove')
                if target.exists():
                    target.rmdir()

            with mock.patch.dict(
                measure_once.__globals__,
                {
                    'filesystem_observation': lambda _path: dict(filesystem),
                    'add_worktree': add,
                    'remove_worktree': remove,
                    'rusage_snapshot': lambda: (0.0, 0.0),
                },
            ):
                sample = measure_once(plan, identity, pnpm, 1)

        self.assertEqual(sample['result'], 'failed')
        self.assertEqual(sample['failure'], 'partial_registration')
        self.assertEqual(events, ['add', 'remove'])

    def test_summary_publishes_p50_p90_p99_from_successful_samples_only(self) -> None:
        summarize = NAMESPACE['summarize']
        samples = []
        for ordinal, value in enumerate((1.0, 2.0, 3.0, 4.0, 100.0), 1):
            samples.append(
                {
                    'ordinal': ordinal,
                    'result': 'succeeded' if ordinal < 5 else 'failed',
                    'workspace_ready_seconds': value,
                    'observed_import_mechanisms': ['reflink_observed'],
                }
            )
        result = summarize(samples)
        distribution = result['distributions']['workspace_ready_seconds']
        self.assertEqual(result['successful_samples'], 4)
        self.assertEqual(result['failed_samples'], 1)
        self.assertEqual(distribution['p50'], 2.5)
        self.assertAlmostEqual(distribution['p90'], 3.7)
        self.assertAlmostEqual(distribution['p99'], 3.97)
        self.assertEqual(
            result['observed_import_mechanism_sample_counts'],
            {'reflink_observed': 4},
        )

    def test_worktree_cleanup_accepts_only_exact_unregistered_target(self) -> None:
        remove = NAMESPACE['remove_worktree']
        BenchmarkError = NAMESPACE['BenchmarkError']
        failed_remove = SimpleNamespace(returncode=128, stdout=b'')
        absent = SimpleNamespace(
            returncode=0,
            stdout=b'worktree /different\0HEAD 0000\0\0',
        )
        with mock.patch.object(
            remove.__globals__['subprocess'],
            'run',
            side_effect=(failed_remove, absent),
        ):
            remove(Path('/repository'), Path('/target'))

        present = SimpleNamespace(
            returncode=0,
            stdout=b'worktree /target\0HEAD 0000\0\0',
        )
        with mock.patch.object(
            remove.__globals__['subprocess'],
            'run',
            side_effect=(failed_remove, present),
        ):
            with self.assertRaises(BenchmarkError):
                remove(Path('/repository'), Path('/target'))

    def test_cohort_interruption_terminates_process_group_before_reraising(self) -> None:
        run_cohort = NAMESPACE['run_cohort']
        process_group_exists = NAMESPACE['process_group_exists']
        real_sleep = time.sleep
        interrupted = False
        process_group_ids: list[int] = []
        real_terminate = run_cohort.__globals__['terminate_processes']

        def interrupt_once(seconds: float) -> None:
            nonlocal interrupted
            if not interrupted:
                interrupted = True
                raise KeyboardInterrupt
            real_sleep(seconds)

        def terminate(processes: list[object]) -> None:
            process_group_ids.extend(process.pid for process in processes)
            real_terminate(processes)

        with mock.patch.object(
            run_cohort.__globals__['time'],
            'sleep',
            side_effect=interrupt_once,
        ), mock.patch.dict(
            run_cohort.__globals__,
            {'terminate_processes': terminate},
        ):
            with self.assertRaises(KeyboardInterrupt):
                run_cohort(
                    [(Path('/tmp'), ['/usr/bin/python3', '-c', 'import time; time.sleep(60)'])],
                    env={
                        'PATH': '/usr/bin:/bin',
                        'LANG': 'C.UTF-8',
                        'LC_ALL': 'C.UTF-8',
                    },
                    deadline_seconds=60,
                )

        self.assertTrue(process_group_ids)
        self.assertTrue(all(not process_group_exists(pid) for pid in process_group_ids))

    def test_cohort_deadline_owns_and_terminates_process_group(self) -> None:
        run_cohort = NAMESPACE['run_cohort']
        program = (
            'import signal,time;'
            'signal.signal(signal.SIGTERM,signal.SIG_IGN);'
            'time.sleep(60)'
        )
        started = time.monotonic()
        with mock.patch.dict(
            run_cohort.__globals__,
            {
                'PROCESS_GROUP_TERM_GRACE_SECONDS': 0.05,
                'PROCESS_GROUP_KILL_GRACE_SECONDS': 1.0,
                'PROCESS_GROUP_POLL_SECONDS': 0.01,
            },
        ):
            result = run_cohort(
                [(Path('/tmp'), ['/usr/bin/python3', '-c', program])],
                env={'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8', 'LC_ALL': 'C.UTF-8'},
                deadline_seconds=0.05,
            )
        self.assertTrue(result['deadline_exceeded'])
        self.assertGreaterEqual(result['failure_count'], 1)
        self.assertLess(time.monotonic() - started, 2.0)

    def test_leader_first_descendant_is_detected_and_killed(self) -> None:
        run_cohort = NAMESPACE['run_cohort']
        descendant = (
            'import signal,time;'
            'signal.signal(signal.SIGTERM,signal.SIG_IGN);'
            'time.sleep(60)'
        )
        leader = (
            'import subprocess;'
            "subprocess.Popen(['/usr/bin/python3','-c'," + repr(descendant) + ']);'
        )
        with mock.patch.dict(
            run_cohort.__globals__,
            {
                'PROCESS_GROUP_TERM_GRACE_SECONDS': 0.05,
                'PROCESS_GROUP_KILL_GRACE_SECONDS': 1.0,
                'PROCESS_GROUP_POLL_SECONDS': 0.01,
            },
        ):
            result = run_cohort(
                [(Path('/tmp'), ['/usr/bin/python3', '-c', leader])],
                env={'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8', 'LC_ALL': 'C.UTF-8'},
                deadline_seconds=1,
            )
        self.assertEqual(result['surviving_process_groups_detected'], 1)
        self.assertGreaterEqual(result['failure_count'], 1)


if __name__ == '__main__':
    unittest.main()
