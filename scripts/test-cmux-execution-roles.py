#!/usr/bin/env python3
from __future__ import annotations

import copy
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = Path(__file__).with_name('cmux_execution_roles.py')
SPEC = importlib.util.spec_from_file_location('cmux_execution_roles', MODULE_PATH)
assert SPEC and SPEC.loader
m = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(m)

GEN = 'sha256:' + 'a' * 64
PREF = 'sha256:' + 'b' * 64


def fleet_acceptance(node_id, enrollment_generation, role):
    return {
        'schema': m.fleet.ACCEPTANCE_SCHEMA,
        'nodeId': node_id,
        'enrollmentGeneration': enrollment_generation,
        'role': role,
        'source': {
            'repository': m.fleet.CMUX_REPOSITORY,
            'commit': '1' * 40,
            'tree': '2' * 40,
        },
        'profile': dict(m.fleet.ROLE_PROFILES[role]),
        'toolchainGeneration': GEN,
        'glaedaGeneration': PREF,
        'glaedaFleetContractGeneration': m.fleet.fleet_contract_generation(),
        'cmuxSemanticResultSha256': GEN,
        'cmuxSemanticResultState': 'passed',
        'cmuxEnvironmentClass': 'fixture',
        'cmuxToolchainIdentity': GEN,
        'postBootstrapSha256': GEN,
        'executionClass': m.fleet.LOCAL_EXECUTION_CLASS,
        'localExecutionAttemptSha256': GEN,
        'processSettlement': 'complete',
        'result': 'accepted',
    }


def fleet_binding(node_id, enrollment_generation, role):
    return m.fleet_role_acceptances(
        node_id,
        enrollment_generation,
        [fleet_acceptance(node_id, enrollment_generation, role)],
    )[role]


def mac_node(state='eligible', generation=7, xcode='apple-xcode-26-sdk-26', memory='large', pressure=None):
    return {
        'schema': m.NODE_SCHEMA,
        'nodeId': 'cmux-mac-001',
        'state': state,
        'platform': 'macos',
        'architecture': 'arm64',
        'osVersionClass': 'macos-26',
        'enrollmentGeneration': 3,
        'cpuClass': 'large',
        'memoryClass': memory,
        'diskClass': 'large',
        'capabilityGeneration': generation,
        'toolchainProfiles': [xcode],
        'capabilities': sorted({
            'apple_silicon',
            'app_host_test',
            'benchmark',
            'diagnostic',
            'native_glaeda_build',
            'native_release_build',
            'native_xcode_build',
        }),
        'roleAcceptances': {
            'cmux_macos_native_build': fleet_binding(
                'cmux-mac-001',
                3,
                'cmux_macos_native_build',
            ),
        },
        'pressure': pressure or {
            'cpu': 'normal',
            'memory': 'normal',
            'swap': 'none',
            'thermal': 'stable',
        },
    }


def linux_node(state='eligible', generation=4, memory='large'):
    return {
        'schema': m.NODE_SCHEMA,
        'nodeId': 'cmux-linux-001',
        'state': state,
        'platform': 'linux',
        'architecture': 'x86_64',
        'osVersionClass': 'ubuntu-24.04',
        'enrollmentGeneration': 2,
        'cpuClass': 'large',
        'memoryClass': memory,
        'diskClass': 'large',
        'capabilityGeneration': generation,
        'toolchainProfiles': ['linux-rust-ci-2026-09'],
        'capabilities': sorted({
            'artifact_service',
            'background_replay',
            'background_verification',
            'benchmark',
            'build_helper',
            'cgroup_v2',
            'diagnostic',
            'linux_agent',
            'linux_ci',
            'linux_ci_toolchain',
            'systemd_execution',
            'task_isolation',
            'web_ci',
        }),
        'roleAcceptances': {
            'cmux_linux_ci': fleet_binding(
                'cmux-linux-001',
                2,
                'cmux_linux_ci',
            ),
        },
        'pressure': {
            'cpu': 'normal',
            'memory': 'normal',
            'swap': 'none',
            'thermal': 'stable',
        },
    }


def canary(
    node,
    role,
    *,
    result='accepted',
    toolchain=None,
    capabilities=None,
    profiles=None,
    profile=None,
    acceptance_digest=None,
):
    if capabilities is None:
        capabilities = set(m.ROLE_REQUIREMENTS[role]['requiredCapabilities'])
    if role == 'cmux_macos_native_build':
        capabilities |= {'native_release_build'}
    if role == 'cmux_macos_test':
        capabilities |= {'app_host_test'}
    if role == 'cmux_linux_ci':
        capabilities |= {'linux_ci', 'web_ci', 'background_verification'}
    if role == 'cmux_linux_agent':
        capabilities |= {'linux_agent', 'build_helper'}
    current = node.get('roleAcceptances', {}).get(role)
    selected_profile = profile or (
        copy.deepcopy(current['profile'])
        if current is not None
        else {'id': 'reserved.placeholder', 'generation': 1}
    )
    selected_digest = acceptance_digest or (
        current['receiptSha256'] if current is not None else PREF
    )
    return {
        'schema': m.ROLE_CANARY_SCHEMA,
        'nodeId': node['nodeId'],
        'enrollmentGeneration': node['enrollmentGeneration'],
        'capabilityGeneration': node['capabilityGeneration'],
        'role': role,
        'toolchainProfile': toolchain if toolchain is not None else (
            node['toolchainProfiles'][0] if m.ROLE_REQUIREMENTS[role]['requiresToolchainProfile'] else None
        ),
        'acceptedCapabilities': sorted(capabilities),
        'acceptedResourceProfiles': profiles or ['large', 'medium', 'small'],
        'profile': selected_profile,
        'acceptanceReceiptSha256': selected_digest,
        'result': result,
    }


def capacity(node, role, profile, slot, concurrent, *, result='accepted', pressure='normal', unfinished=0, generation=GEN):
    return {
        'schema': m.CAPACITY_SCHEMA,
        'nodeId': node['nodeId'],
        'enrollmentGeneration': node['enrollmentGeneration'],
        'capabilityGeneration': node['capabilityGeneration'],
        'role': role,
        'resourceProfile': profile,
        'slotClass': slot,
        'maxConcurrent': concurrent,
        'contentionEvidenceGeneration': generation,
        'measurement': {
            'offeredTasks': 8,
            'maximumSimultaneous': concurrent,
            'startedTasks': 8,
            'settledTasks': 8 - unfinished,
            'validatedCompletions': 8 - unfinished,
            'p50Millis': 1000,
            'p90Millis': 1400,
            'cpuPressure': pressure,
            'memoryPressure': pressure,
            'swapStartBytes': 0,
            'swapPeakBytes': 0,
            'swapEndBytes': 0,
            'thermalBehavior': 'stable',
            'unfinishedWork': unfinished,
        },
        'result': result,
    }


def mac_compile_capacities(node, profile='medium', *, build=1, heavy=2):
    return [
        capacity(node, 'cmux_macos_native_build', profile, 'mac_native_build_lane', build),
        capacity(node, 'cmux_macos_native_build', profile, 'mac_native_heavy_slot', heavy),
    ]


def mac_test_capacities(node, profile='medium', *, tests=2, heavy=2):
    return [
        capacity(node, 'cmux_macos_test', profile, 'mac_app_host_test_slot', tests),
        capacity(node, 'cmux_macos_test', profile, 'mac_native_heavy_slot', heavy),
    ]


def physical_lease(node, lease_id, slot_claims, *, owner='cmux-ci', state='active', exclusive=False):
    return {
        'schema': m.PHYSICAL_LEASE_SCHEMA,
        'nodeId': node['nodeId'],
        'leaseId': lease_id,
        'leaseGeneration': 1,
        'ownerNamespace': owner,
        'state': state,
        'slotClaims': sorted(slot_claims),
        'exclusive': exclusive,
    }


def workload(operation, role, platform, architecture, capabilities, *, toolchain=None, memory='medium', profile='medium'):
    return {
        'schema': m.WORKLOAD_SCHEMA,
        'operation': operation,
        'role': role,
        'platform': platform,
        'architecture': architecture,
        'toolchainProfile': toolchain,
        'minimumCpuClass': 'medium',
        'minimumMemoryClass': memory,
        'requiredCapabilities': sorted(capabilities),
        'resourceProfile': profile,
    }


class RoleModelTests(unittest.TestCase):
    def test_closed_role_vocabulary_keeps_workload_specialization_as_capabilities(self):
        self.assertEqual(
            set(m.ROLES),
            {
                'artifact_cache',
                'background_replay',
                'benchmark',
                'cmux_linux_agent',
                'cmux_linux_ci',
                'cmux_macos_native_build',
                'cmux_macos_test',
                'diagnostic',
            },
        )
        self.assertEqual(
            set(m.CAPABILITY_CLASSES),
            {
                'app_host_test', 'apple_silicon', 'artifact_service', 'background_replay',
                'background_verification', 'benchmark', 'build_helper', 'cgroup_v2',
                'diagnostic', 'linux_agent', 'linux_ci', 'linux_ci_toolchain',
                'native_glaeda_build', 'native_release_build', 'native_xcode_build',
                'systemd_execution', 'task_isolation', 'web_ci',
            },
        )
        self.assertEqual(m.OPERATION_CONTRACTS['cmux_macos_release_admission']['role'], 'cmux_macos_native_build')
        self.assertIn('native_release_build', m.OPERATION_CONTRACTS['cmux_macos_release_admission']['requiredCapabilities'])
        self.assertEqual(m.OPERATION_CONTRACTS['cmux_web_ci_admission']['role'], 'cmux_linux_ci')
        self.assertEqual(m.OPERATION_CONTRACTS['cmux_native_dev_build_admission']['role'], 'cmux_macos_native_build')
        self.assertEqual(m.OPERATION_CONTRACTS['artifact_cache_publish']['slotClaims'], ('artifact_publisher_slot',))

    def test_node_meets_exact_mac_role_and_workload_requirements(self):
        node = mac_node()
        canaries = [canary(node, 'cmux_macos_native_build')]
        caps = mac_compile_capacities(node)
        req = workload(
            'cmux_macos_compile_admission',
            'cmux_macos_native_build',
            'macos',
            'arm64',
            {'native_xcode_build'},
            toolchain='apple-xcode-26-sdk-26',
        )
        self.assertEqual(m.select_eligible(req, node, canaries, caps), (True, 'eligible'))
        decision = m.local_admission(req, node, canaries, caps, [])
        self.assertTrue(decision['accepted'])
        self.assertEqual(decision['authority'], 'admission_only')
        self.assertEqual(
            decision['slotClaims'],
            ['mac_native_build_lane', 'mac_native_heavy_slot'],
        )

    def test_wrong_xcode_profile_refuses_cleanly(self):
        node = mac_node()
        canaries = [canary(node, 'cmux_macos_native_build')]
        caps = mac_compile_capacities(node)
        req = workload(
            'cmux_macos_compile_admission',
            'cmux_macos_native_build',
            'macos',
            'arm64',
            {'native_xcode_build'},
            toolchain='apple-xcode-27-sdk-27',
        )
        self.assertEqual(m.select_eligible(req, node, canaries, caps), (False, 'toolchain_profile_missing'))

    def test_insufficient_memory_class_refuses(self):
        node = mac_node(memory='small')
        canaries = [canary(node, 'cmux_macos_native_build')]
        eligibility = m.role_eligibility(node, canaries)
        self.assertEqual(eligibility['cmux_macos_native_build']['reason'], 'insufficient_memory_class')

    def test_insufficient_cpu_class_refuses_linux_role(self):
        node = linux_node()
        node['cpuClass'] = 'small'
        canaries = [canary(node, 'cmux_linux_ci')]
        eligibility = m.role_eligibility(node, canaries)
        self.assertEqual(eligibility['cmux_linux_ci']['reason'], 'insufficient_cpu_class')

    def test_reenrollment_invalidates_old_role_acceptance_and_canary(self):
        node = mac_node()
        receipt = canary(node, 'cmux_macos_native_build')
        node['enrollmentGeneration'] += 1
        with self.assertRaisesRegex(
            m.RoleModelError,
            'role acceptance enrollment generation is stale',
        ):
            m.role_eligibility(node, [receipt])

        node['roleAcceptances'] = {}
        eligibility = m.role_eligibility(node, [receipt])
        self.assertEqual(
            eligibility['cmux_macos_native_build']['reason'],
            'fleet_acceptance_pending',
        )

    def test_role_canary_binds_exact_current_fleet_acceptance(self):
        node = mac_node()
        receipt = canary(node, 'cmux_macos_native_build')
        self.assertEqual(
            receipt['acceptanceReceiptSha256'],
            node['roleAcceptances']['cmux_macos_native_build']['receiptSha256'],
        )
        self.assertEqual(
            receipt['profile'],
            m.fleet.ROLE_PROFILES['cmux_macos_native_build'],
        )

        stale_digest = copy.deepcopy(receipt)
        stale_digest['acceptanceReceiptSha256'] = GEN
        eligibility = m.role_eligibility(node, [stale_digest])
        self.assertEqual(
            eligibility['cmux_macos_native_build']['reason'],
            'role_canary_acceptance_stale',
        )

        stale_profile = copy.deepcopy(receipt)
        stale_profile['profile']['generation'] += 1
        eligibility = m.role_eligibility(node, [stale_profile])
        self.assertEqual(
            eligibility['cmux_macos_native_build']['reason'],
            'role_canary_profile_stale',
        )

    def test_fleet_acceptance_projection_rejects_invalid_receipt(self):
        receipt = fleet_acceptance(
            'cmux-mac-001',
            3,
            'cmux_macos_native_build',
        )
        receipt['cmuxSemanticResultState'] = 'failed'
        with self.assertRaisesRegex(
            m.RoleModelError,
            'fleet acceptance receipt is invalid',
        ):
            m.fleet_acceptance_binding(receipt)

    def test_fleet_acceptance_projection_rejects_wrong_node_and_generation(self):
        wrong_node = fleet_acceptance(
            'cmux-other-001',
            3,
            'cmux_macos_native_build',
        )
        with self.assertRaisesRegex(
            m.RoleModelError,
            'different node',
        ):
            m.fleet_role_acceptances('cmux-mac-001', 3, [wrong_node])

        stale = fleet_acceptance(
            'cmux-mac-001',
            2,
            'cmux_macos_native_build',
        )
        with self.assertRaisesRegex(
            m.RoleModelError,
            'generation is stale',
        ):
            m.fleet_role_acceptances('cmux-mac-001', 3, [stale])

    def test_fleet_acceptance_projection_rejects_duplicate_role(self):
        receipt = fleet_acceptance(
            'cmux-mac-001',
            3,
            'cmux_macos_native_build',
        )
        with self.assertRaisesRegex(
            m.RoleModelError,
            'duplicate fleet acceptance role',
        ):
            m.fleet_role_acceptances(
                'cmux-mac-001',
                3,
                [receipt, copy.deepcopy(receipt)],
            )

    def test_reserved_role_cannot_install_fake_current_acceptance(self):
        node = mac_node()
        node['roleAcceptances']['cmux_macos_test'] = {
            'nodeId': node['nodeId'],
            'enrollmentGeneration': node['enrollmentGeneration'],
            'profile': {'id': 'cmux.macos.app-host-test-shard', 'generation': 1},
            'receiptSha256': GEN,
        }
        with self.assertRaisesRegex(
            m.RoleModelError,
            'not reviewed by fleet enrollment',
        ):
            m.validate_node(node)

    def test_failed_role_canary_refuses(self):
        node = mac_node()
        eligibility = m.role_eligibility(node, [canary(node, 'cmux_macos_native_build', result='rejected')])
        self.assertEqual(eligibility['cmux_macos_native_build']['reason'], 'role_canary_failed')

    def test_draining_node_refuses_every_role(self):
        node = mac_node(state='draining')
        eligibility = m.role_eligibility(node, [canary(node, 'cmux_macos_native_build')])
        self.assertEqual(eligibility['cmux_macos_native_build']['reason'], 'node_draining')

    def test_role_is_removed_after_upgrade_until_recanary(self):
        old = mac_node(generation=7)
        receipt = canary(old, 'cmux_macos_native_build')
        upgraded = mac_node(generation=8, xcode='apple-xcode-27-sdk-27')
        eligibility = m.role_eligibility(upgraded, [receipt])
        self.assertEqual(eligibility['cmux_macos_native_build']['reason'], 'role_canary_pending')

    def test_future_mac_test_role_stays_pending_without_fleet_acceptance(self):
        node = mac_node()
        canaries = [
            canary(node, 'cmux_macos_native_build'),
            canary(node, 'cmux_macos_test'),
        ]
        roles = m.role_eligibility(node, canaries)
        self.assertTrue(roles['cmux_macos_native_build']['eligible'])
        self.assertFalse(roles['cmux_macos_test']['eligible'])
        self.assertEqual(
            roles['cmux_macos_test']['reason'],
            'fleet_acceptance_pending',
        )

        req = workload(
            'cmux_macos_app_host_test',
            'cmux_macos_test',
            'macos',
            'arm64',
            {'app_host_test'},
            toolchain='apple-xcode-26-sdk-26',
        )
        decision = m.local_admission(
            req,
            node,
            canaries,
            mac_test_capacities(node),
            [],
        )
        self.assertFalse(decision['accepted'])
        self.assertEqual(decision['reason'], 'fleet_acceptance_pending')

    def test_multi_slot_capacity_requires_one_complete_evidence_generation(self):
        node = mac_node()
        canaries = [canary(node, 'cmux_macos_native_build')]
        req = workload(
            'cmux_macos_compile_admission',
            'cmux_macos_native_build',
            'macos',
            'arm64',
            {'native_xcode_build'},
            toolchain='apple-xcode-26-sdk-26',
        )
        split = [
            capacity(
                node,
                'cmux_macos_native_build',
                'medium',
                'mac_native_build_lane',
                1,
                generation=GEN,
            ),
            capacity(
                node,
                'cmux_macos_native_build',
                'medium',
                'mac_native_heavy_slot',
                2,
                generation=PREF,
            ),
        ]
        self.assertEqual(
            m.select_eligible(req, node, canaries, split),
            (False, 'resource_profile_unmeasured'),
        )

        complete = mac_compile_capacities(node)
        self.assertEqual(
            m.select_eligible(req, node, canaries, complete),
            (True, 'eligible'),
        )

    def test_same_capacity_generation_cannot_disagree_on_one_slot(self):
        node = linux_node()
        canaries = [canary(node, 'cmux_linux_ci')]
        req = workload(
            'cmux_linux_ci_admission',
            'cmux_linux_ci',
            'linux',
            'x86_64',
            {'linux_ci'},
            toolchain='linux-rust-ci-2026-09',
        )
        evidence = [
            capacity(
                node,
                'cmux_linux_ci',
                'medium',
                'linux_medium_slot',
                4,
            ),
            capacity(
                node,
                'cmux_linux_ci',
                'medium',
                'linux_medium_slot',
                3,
            ),
        ]
        with self.assertRaisesRegex(
            m.RoleModelError,
            'disagrees on slot capacity',
        ):
            m.select_eligible(req, node, canaries, evidence)

    def test_four_linux_medium_jobs_are_the_measured_limit(self):
        node = linux_node()
        canaries = [canary(node, 'cmux_linux_ci')]
        caps = [capacity(node, 'cmux_linux_ci', 'medium', 'linux_medium_slot', 4)]
        req = workload(
            'cmux_linux_ci_admission', 'cmux_linux_ci', 'linux', 'x86_64',
            {'linux_ci'}, toolchain='linux-rust-ci-2026-09'
        )
        leases = [
            physical_lease(node, f'linux-{index}', ['linux_medium_slot'], owner=f'caller-{index}')
            for index in range(4)
        ]
        self.assertTrue(m.local_admission(req, node, canaries, caps, leases[:3])['accepted'])
        refused = m.local_admission(req, node, canaries, caps, leases)
        self.assertFalse(refused['accepted'])
        self.assertEqual(refused['reason'], 'physical_slot_unavailable')

    def test_unsettled_physical_lease_keeps_slot_busy(self):
        node = mac_node()
        canaries = [canary(node, 'cmux_macos_native_build')]
        caps = mac_compile_capacities(node)
        req = workload(
            'cmux_macos_compile_admission', 'cmux_macos_native_build', 'macos', 'arm64',
            {'native_xcode_build'}, toolchain='apple-xcode-26-sdk-26'
        )
        leases = [physical_lease(
            node,
            'compile-unsettled',
            ['mac_native_build_lane'],
            owner='github-actions',
            state='unsettled',
        )]
        decision = m.local_admission(req, node, canaries, caps, leases)
        self.assertFalse(decision['accepted'])
        self.assertEqual(decision['reason'], 'physical_slot_unavailable')

    def test_exclusive_profile_uses_physical_lease_boundary(self):
        node = mac_node()
        canaries = [canary(node, 'cmux_macos_native_build', profiles=['exclusive'])]
        caps = mac_compile_capacities(node, 'exclusive', build=1, heavy=1)
        req = workload(
            'cmux_macos_compile_admission', 'cmux_macos_native_build', 'macos', 'arm64',
            {'native_xcode_build'}, toolchain='apple-xcode-26-sdk-26', profile='exclusive'
        )
        leases = [physical_lease(node, 'other-work', [], owner='direct-agent')]
        decision = m.local_admission(req, node, canaries, caps, leases)
        self.assertFalse(decision['accepted'])
        self.assertEqual(decision['reason'], 'physical_exclusive_unavailable')

    def test_external_scheduler_cannot_request_ineligible_role(self):
        node = linux_node()
        canaries = [canary(node, 'cmux_linux_ci')]
        caps = [capacity(node, 'cmux_linux_ci', 'medium', 'linux_medium_slot', 4)]
        req = workload(
            'cmux_linux_agent_admission', 'cmux_linux_agent', 'linux', 'x86_64',
            {'linux_agent'}, toolchain='linux-rust-ci-2026-09'
        )
        decision = m.local_admission(req, node, canaries, caps, [])
        self.assertFalse(decision['accepted'])
        self.assertEqual(decision['reason'], 'fleet_acceptance_pending')

    def test_unknown_fresh_pressure_refuses_local_admission(self):
        node = mac_node()
        canaries = [canary(node, 'cmux_macos_native_build')]
        caps = mac_compile_capacities(node)
        req = workload(
            'cmux_macos_compile_admission', 'cmux_macos_native_build', 'macos', 'arm64',
            {'native_xcode_build'}, toolchain='apple-xcode-26-sdk-26'
        )
        node['pressure']['memory'] = 'unknown'
        decision = m.local_admission(req, node, canaries, caps, [])
        self.assertFalse(decision['accepted'])
        self.assertEqual(decision['reason'], 'node_pressured')

    def test_node_pressure_between_selection_and_local_admission_vetoes(self):
        selected = mac_node()
        canaries = [canary(selected, 'cmux_macos_native_build')]
        caps = mac_compile_capacities(selected)
        req = workload(
            'cmux_macos_compile_admission', 'cmux_macos_native_build', 'macos', 'arm64',
            {'native_xcode_build'}, toolchain='apple-xcode-26-sdk-26'
        )
        self.assertEqual(m.select_eligible(req, selected, canaries, caps), (True, 'eligible'))
        pressured = copy.deepcopy(selected)
        pressured['pressure']['memory'] = 'critical'
        decision = m.local_admission(req, pressured, canaries, caps, [])
        self.assertFalse(decision['accepted'])
        self.assertEqual(decision['reason'], 'node_pressured')

    def test_resource_profile_requires_reviewed_measurement(self):
        node = linux_node()
        canaries = [canary(node, 'cmux_linux_ci')]
        req = workload(
            'cmux_linux_ci_admission', 'cmux_linux_ci', 'linux', 'x86_64',
            {'linux_ci'}, toolchain='linux-rust-ci-2026-09', profile='large'
        )
        caps = [capacity(node, 'cmux_linux_ci', 'medium', 'linux_medium_slot', 4)]
        self.assertEqual(m.select_eligible(req, node, canaries, caps), (False, 'resource_profile_unmeasured'))

    def test_background_role_stays_pending_until_fleet_acceptance_exists(self):
        node = linux_node()
        canaries = [canary(node, 'background_replay', profiles=['small'])]
        req = workload(
            'background_replay', 'background_replay', 'linux', 'x86_64',
            {'background_replay'}, profile='small', memory='small'
        )
        req['minimumCpuClass'] = 'small'
        evidence = [
            capacity(
                node,
                'background_replay',
                'small',
                'background_replay_slot',
                4,
            )
        ]
        self.assertEqual(
            m.select_eligible(req, node, canaries, evidence),
            (False, 'fleet_acceptance_pending'),
        )

    def test_accepted_capacity_refuses_unknown_or_critical_pressure(self):
        node = linux_node()
        for pressure in ('unknown', 'critical'):
            with self.subTest(pressure=pressure):
                receipt = capacity(
                    node, 'cmux_linux_ci', 'medium', 'linux_medium_slot', 4,
                    pressure=pressure,
                )
                with self.assertRaisesRegex(m.RoleModelError, 'bounded .* pressure'):
                    m.validate_capacity(receipt)

    def test_accepted_capacity_requires_validated_completion(self):
        node = linux_node()
        receipt = capacity(node, 'cmux_linux_ci', 'medium', 'linux_medium_slot', 4)
        receipt['measurement']['validatedCompletions'] = 0
        with self.assertRaisesRegex(m.RoleModelError, 'requires validated completions'):
            m.validate_capacity(receipt)

    def test_unknown_physical_slot_class_refuses(self):
        node = mac_node()
        lease = physical_lease(node, 'bad-slot', ['mac_native_build_lane'])
        lease['slotClaims'] = ['caller_invented_slot']
        with self.assertRaisesRegex(m.RoleModelError, 'unsupported value'):
            m.validate_physical_lease(lease)

    def test_preference_is_operation_specific(self):
        node = mac_node()
        preference = {
            'schema': m.PREFERENCE_SCHEMA,
            'nodeId': node['nodeId'],
            'role': 'cmux_macos_native_build',
            'operation': 'cmux_macos_compile_admission',
            'evidenceGeneration': PREF,
            'preference': 'preferred',
            'authority': 'observation_only',
        }
        m.validate_preference(preference)
        preference['operation'] = 'cmux_web_ci_admission'
        with self.assertRaisesRegex(m.RoleModelError, 'operation and role disagree'):
            m.validate_preference(preference)

    def test_accepted_capacity_requires_all_offered_work_settled(self):
        node = linux_node()
        receipt = capacity(
            node,
            'cmux_linux_ci',
            'medium',
            'linux_medium_slot',
            4,
            unfinished=1,
        )
        with self.assertRaisesRegex(
            m.RoleModelError,
            'all offered work settled',
        ):
            m.validate_capacity(receipt)

        receipt['result'] = 'rejected'
        self.assertEqual(
            m.validate_capacity(receipt)['measurement']['unfinishedWork'],
            1,
        )

    def test_accepted_capacity_cannot_hide_settled_unvalidated_work(self):
        node = linux_node()
        receipt = capacity(
            node,
            'cmux_linux_ci',
            'medium',
            'linux_medium_slot',
            4,
        )
        receipt['measurement']['validatedCompletions'] = 7
        with self.assertRaisesRegex(
            m.RoleModelError,
            'every settled task validated',
        ):
            m.validate_capacity(receipt)

    def test_accepted_capacity_cannot_exceed_measured_simultaneous_work(self):
        node = linux_node()
        receipt = capacity(
            node,
            'cmux_linux_ci',
            'medium',
            'linux_medium_slot',
            4,
        )
        receipt['measurement']['maximumSimultaneous'] = 3
        with self.assertRaisesRegex(
            m.RoleModelError,
            'exceeds measured simultaneous work',
        ):
            m.validate_capacity(receipt)

    def test_capacity_cohort_counts_must_reconcile(self):
        node = linux_node()
        receipt = capacity(
            node,
            'cmux_linux_ci',
            'medium',
            'linux_medium_slot',
            4,
        )
        receipt['measurement']['settledTasks'] = 7
        receipt['measurement']['validatedCompletions'] = 7
        with self.assertRaisesRegex(
            m.RoleModelError,
            'unfinished work disagrees',
        ):
            m.validate_capacity(receipt)

    def test_capacity_receipt_records_contention_window_fields_without_remote_raw_cpu_ram(self):
        node = linux_node()
        receipt = capacity(node, 'cmux_linux_ci', 'medium', 'linux_medium_slot', 4)
        validated = m.validate_capacity(receipt)
        measurement = validated['measurement']
        self.assertEqual(validated['contentionEvidenceGeneration'], GEN)
        self.assertEqual(
            set(measurement),
            {
                'offeredTasks', 'maximumSimultaneous', 'startedTasks',
                'settledTasks', 'validatedCompletions', 'p50Millis', 'p90Millis',
                'cpuPressure', 'memoryPressure', 'swapStartBytes', 'swapPeakBytes',
                'swapEndBytes', 'thermalBehavior', 'unfinishedWork'
            },
        )
        self.assertNotIn('cpu', validated)
        self.assertNotIn('ramBytes', validated)
        self.assertNotIn('cgroup', validated)

    def test_only_current_fleet_accepted_roles_are_eligible(self):
        mac = mac_node()
        mac_roles = m.role_eligibility(mac, [
            canary(mac, 'cmux_macos_native_build'),
            canary(mac, 'cmux_macos_test'),
        ])
        self.assertTrue(mac_roles['cmux_macos_native_build']['eligible'])
        self.assertEqual(
            mac_roles['cmux_macos_test']['reason'],
            'fleet_acceptance_pending',
        )

        linux = linux_node()
        linux_roles = m.role_eligibility(linux, [
            canary(linux, 'cmux_linux_ci'),
            canary(linux, 'cmux_linux_agent'),
        ])
        self.assertTrue(linux_roles['cmux_linux_ci']['eligible'])
        self.assertEqual(
            linux_roles['cmux_linux_agent']['reason'],
            'fleet_acceptance_pending',
        )

    def test_preferred_is_advisory_and_separate_from_eligibility(self):
        node = mac_node()
        canaries = [canary(node, 'cmux_macos_test')]
        preference = {
            'schema': m.PREFERENCE_SCHEMA,
            'nodeId': node['nodeId'],
            'role': 'cmux_macos_native_build',
            'operation': 'cmux_macos_compile_admission',
            'evidenceGeneration': PREF,
            'preference': 'preferred',
            'authority': 'observation_only',
        }
        status = m.operator_status(node, canaries, [], [preference])
        self.assertIn('cmux_macos_compile_admission', status['preferred'])
        self.assertNotIn('cmux_macos_native_build', status['eligible'])
        background = dict(preference)
        background['role'] = 'background_replay'
        background['operation'] = 'background_replay'
        background['preference'] = 'background'
        status = m.operator_status(node, canaries, [], [preference, background])
        self.assertIn('background_replay', status['background'])

    def test_compact_operator_status_hides_internal_objects(self):
        node = mac_node(state='active')
        canaries = [canary(node, 'cmux_macos_native_build')]
        caps = mac_compile_capacities(node)
        status = m.operator_status(node, canaries, caps)
        human = m.render_status(status)
        self.assertIn('node cmux-mac-001', human)
        self.assertIn('cmux_macos_native_build', human)
        self.assertIn('mac_native_build_lane: 1', human)
        self.assertIn('state: active', human)
        self.assertNotIn('capabilityGeneration', human)

    def test_unknown_workload_capability_refuses(self):
        req = workload(
            'cmux_macos_compile_admission', 'cmux_macos_native_build', 'macos', 'arm64',
            {'native_xcode_build'}, toolchain='apple-xcode-26-sdk-26'
        )
        req['requiredCapabilities'] = ['caller_invented_capability', 'native_xcode_build']
        with self.assertRaisesRegex(m.RoleModelError, 'unsupported value'):
            m.validate_workload(req)

    def test_cost_and_machine_age_are_not_placement_inputs(self):
        req = workload(
            'cmux_linux_ci_admission', 'cmux_linux_ci', 'linux', 'x86_64',
            {'linux_ci'}, toolchain='linux-rust-ci-2026-09'
        )
        for field, value in (('costClass', 'cheap'), ('machineAge', 'new')):
            with self.subTest(field=field):
                candidate = copy.deepcopy(req)
                candidate[field] = value
                with self.assertRaisesRegex(m.RoleModelError, 'unknown or missing fields'):
                    m.validate_workload(candidate)

    def test_workload_has_no_machine_selector_or_raw_resource_controls(self):
        req = workload(
            'cmux_macos_compile_admission', 'cmux_macos_native_build', 'macos', 'arm64',
            {'native_xcode_build'}, toolchain='apple-xcode-26-sdk-26'
        )
        m.validate_workload(req)
        self.assertNotIn('nodeId', req)
        self.assertNotIn('cpu', req)
        self.assertNotIn('ramBytes', req)
        self.assertNotIn('cgroup', req)


if __name__ == '__main__':
    unittest.main()
