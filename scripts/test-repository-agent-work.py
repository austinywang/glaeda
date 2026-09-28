#!/usr/bin/env python3
from __future__ import annotations

import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'scripts/repository_agent_work.py'
SPEC = importlib.util.spec_from_file_location('repository_agent_work', MODULE_PATH)
assert SPEC and SPEC.loader
w = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(w)

FIXTURES = ROOT / 'docs/experiments/repository-agent-work/fixtures-v1.json'
A = 'sha256:' + 'a' * 64
B = 'sha256:' + 'b' * 64
C = 'sha256:' + 'c' * 64
D = 'sha256:' + 'd' * 64


def raw(value: object) -> bytes:
    return w.canonical_bytes(value) + b'\n'


def review_request() -> dict[str, object]:
    return {
        'document_type': w.REQUEST_TYPE,
        'schema_version': 1,
        'operation': 'review',
        'source': {
            'repository': 'manaflow-ai/cmux',
            'head': {'commit': '1' * 40, 'tree': '2' * 40},
            'base': {'commit': '3' * 40, 'tree': '4' * 40},
        },
        'task_contract_sha256': A,
        'scope': {'class': 'changed_paths'},
        'network_class': 'repository_only',
        'mutation': {'class': 'read_only'},
        'verification': [],
    }


def repair_request() -> dict[str, object]:
    return {
        'document_type': w.REQUEST_TYPE,
        'schema_version': 1,
        'operation': 'repair',
        'source': {
            'repository': 'manaflow-ai/cmux',
            'head': {'commit': '1' * 40, 'tree': '2' * 40},
        },
        'task_contract_sha256': B,
        'scope': {
            'class': 'bounded_paths',
            'paths': ['scripts/ci/cmux_workload_profile.py', 'tests/test_ci_workload_profiles.py'],
        },
        'network_class': 'repository_only',
        'mutation': {
            'class': 'task_private_patch',
            'max_changed_paths': 2,
            'max_patch_bytes': 65536,
            'allow_new_files': False,
            'allow_deletes': False,
        },
        'verification': [{'id': 'cmux.ci.guard', 'generation': 1}],
    }


def research_request() -> dict[str, object]:
    return {
        'document_type': w.REQUEST_TYPE,
        'schema_version': 1,
        'operation': 'research',
        'source': {
            'repository': 'manaflow-ai/cmux',
            'head': {'commit': '1' * 40, 'tree': '2' * 40},
        },
        'task_contract_sha256': C,
        'scope': {'class': 'whole_repository'},
        'network_class': 'public_web',
        'mutation': {'class': 'read_only'},
        'verification': [],
    }


def receipt(request: dict[str, object], result: object) -> dict[str, object]:
    return {
        'document_type': w.RECEIPT_TYPE,
        'schema_version': 1,
        'request_sha256': w.request_sha256(w.normalize_request(request)),
        'operation': request['operation'],
        'source': request['source'],
        'state': 'completed',
        'result': result,
        'authority': w.AUTHORITY,
    }


def finding(**updates: object) -> dict[str, object]:
    identity: dict[str, object] = {
        'class': 'correctness',
        'severity': 'high',
        'path': 'scripts/ci/cmux_workload_profile.py',
        'summary': 'Git source identity must ignore inherited Git redirection variables.',
        'evidence_sha256': D,
        'suggested_validation': {'id': 'cmux.ci.guard', 'generation': 1},
        'line_start': 10,
        'line_end': 14,
    }
    identity.update(updates)
    return {'finding_sha256': w.sha256(w.canonical_bytes(identity)), **identity}


class RepositoryAgentWorkTests(unittest.TestCase):
    def test_review_is_read_only_trusted_compute(self) -> None:
        request = w.decode_request(raw(review_request()))
        plan = w.plan(request)
        self.assertEqual(plan['compute_workload']['family'], 'repository_agent_work.v1')
        self.assertEqual(plan['compute_workload']['trust_class'], 'trusted')
        self.assertIn('repository.query', plan['compute_workload']['required_capabilities'])
        self.assertNotIn('repository.task_private_write', plan['compute_workload']['required_capabilities'])
        self.assertEqual(plan['authority'], w.AUTHORITY)

    def test_routing_classification_is_stable_across_exact_review_inputs(self) -> None:
        first_request = w.decode_request(raw(review_request()))
        second_raw = review_request()
        second_raw["source"]["head"] = {
            "commit": "5" * 40,
            "tree": "6" * 40,
        }
        second_raw["source"]["base"] = {
            "commit": "7" * 40,
            "tree": "8" * 40,
        }
        second_raw["task_contract_sha256"] = "sha256:" + "e" * 64
        second_request = w.decode_request(raw(second_raw))

        first = w.plan(first_request)
        second = w.plan(second_request)

        self.assertNotEqual(
            first["compute_workload"]["input_identity"],
            second["compute_workload"]["input_identity"],
        )
        self.assertEqual(
            first["routing_classification"],
            second["routing_classification"],
        )
        self.assertEqual(
            first["routing_classification_sha256"],
            second["routing_classification_sha256"],
        )
        self.assertEqual(
            first["routing_classification_sha256"],
            w.sha256(w.canonical_bytes(first["routing_classification"])),
        )
        self.assertNotIn(
            "input_identity",
            first["routing_classification"],
        )

    def test_routing_classification_separates_repositories(self) -> None:
        first = w.plan(w.decode_request(raw(review_request())))
        other_raw = review_request()
        other_raw["source"]["repository"] = "teamleaderleo/glaeda"
        other = w.plan(w.decode_request(raw(other_raw)))

        self.assertEqual(
            first["routing_classification"]["repository"],
            "manaflow-ai/cmux",
        )
        self.assertEqual(
            other["routing_classification"]["repository"],
            "teamleaderleo/glaeda",
        )
        self.assertNotEqual(
            first["routing_classification_sha256"],
            other["routing_classification_sha256"],
        )

    def test_routing_classification_keeps_coarse_repair_size_classes(self) -> None:
        small_raw = repair_request()
        small_raw["scope"] = {
            "class": "bounded_paths",
            "paths": ["src/a.rs"],
        }
        small_raw["mutation"] = {
            "class": "task_private_patch",
            "max_changed_paths": 1,
            "max_patch_bytes": 16 * 1024,
            "allow_new_files": False,
            "allow_deletes": False,
        }
        small = w.plan(w.decode_request(raw(small_raw)))

        same_class_raw = repair_request()
        same_class_raw["scope"] = {
            "class": "bounded_paths",
            "paths": ["src/other.rs"],
        }
        same_class_raw["mutation"] = copy.deepcopy(small_raw["mutation"])
        same_class = w.plan(w.decode_request(raw(same_class_raw)))

        large_raw = repair_request()
        large_raw["scope"] = {
            "class": "bounded_paths",
            "paths": [f"src/file-{index}.rs" for index in range(20)],
        }
        large_raw["mutation"] = {
            "class": "task_private_patch",
            "max_changed_paths": 32,
            "max_patch_bytes": 1024 * 1024,
            "allow_new_files": True,
            "allow_deletes": True,
        }
        large = w.plan(w.decode_request(raw(large_raw)))

        self.assertEqual(
            small["routing_classification"],
            same_class["routing_classification"],
        )
        self.assertEqual(
            small["routing_classification"]["scope_size_class"],
            "one",
        )
        self.assertEqual(
            small["routing_classification"]["mutation_budget_class"]["patch_bytes"],
            "tiny",
        )
        self.assertNotEqual(
            small["routing_classification"],
            large["routing_classification"],
        )
        self.assertEqual(
            large["routing_classification"]["scope_size_class"],
            "large",
        )
        self.assertEqual(
            large["routing_classification"]["mutation_budget_class"]["patch_bytes"],
            "large",
        )

    def test_routing_classification_keeps_semantic_work_axes(self) -> None:
        review = w.plan(w.decode_request(raw(review_request())))
        repair_raw = repair_request()
        repair = w.plan(w.decode_request(raw(repair_raw)))

        self.assertNotEqual(
            review["routing_classification"],
            repair["routing_classification"],
        )
        self.assertNotEqual(
            review["routing_classification_sha256"],
            repair["routing_classification_sha256"],
        )
        self.assertEqual(
            repair["routing_classification"]["verification_profiles"],
            [{"id": "cmux.ci.guard", "generation": 1}],
        )
        self.assertEqual(
            repair["routing_classification"]["mutation_class"],
            "task_private_patch",
        )

    def test_review_requires_base_and_rejects_public_web_and_mutation(self) -> None:
        request = review_request()
        del request['source']['base']
        with self.assertRaisesRegex(w.ContractRefusal, 'comparison base'):
            w.decode_request(raw(request))
        request = review_request()
        request['network_class'] = 'public_web'
        with self.assertRaisesRegex(w.ContractRefusal, 'public-web'):
            w.decode_request(raw(request))
        request = review_request()
        request['mutation'] = repair_request()['mutation']
        with self.assertRaisesRegex(w.ContractRefusal, 'read-only'):
            w.decode_request(raw(request))

    def test_repair_requires_bounded_mutation_and_exact_verification(self) -> None:
        request = w.decode_request(raw(repair_request()))
        plan = w.plan(request)
        self.assertEqual(plan['compute_workload']['trust_class'], 'ultra_trusted')
        self.assertIn('repository.task_private_write', plan['compute_workload']['required_capabilities'])
        self.assertIn('verification.repository_owned', plan['compute_workload']['required_capabilities'])
        bad = repair_request()
        bad['scope'] = {'class': 'whole_repository'}
        with self.assertRaisesRegex(w.ContractRefusal, 'bounded path'):
            w.decode_request(raw(bad))
        bad = repair_request()
        bad['verification'] = []
        with self.assertRaisesRegex(w.ContractRefusal, 'verification'):
            w.decode_request(raw(bad))

    def test_research_can_use_public_web_but_remains_read_only(self) -> None:
        request = w.decode_request(raw(research_request()))
        plan = w.plan(request)
        self.assertEqual(plan['compute_workload']['trust_class'], 'trusted')
        self.assertIn('network.public', plan['compute_workload']['required_capabilities'])
        bad = research_request()
        bad['mutation'] = repair_request()['mutation']
        with self.assertRaisesRegex(w.ContractRefusal, 'read-only'):
            w.decode_request(raw(bad))

    def test_request_excludes_prompt_transcript_machine_and_shell_controls(self) -> None:
        request = w.decode_request(raw(research_request()))
        encoded = w.canonical_bytes(request)
        for forbidden in [b'prompt', b'transcript', b'reasoning', b'argv', b'cwd', b'machine']:
            self.assertNotIn(forbidden, encoded)
        drift = research_request()
        drift['argv'] = ['sh']
        with self.assertRaisesRegex(w.ContractRefusal, 'unknown or missing'):
            w.decode_request(raw(drift))

    def test_paths_reject_git_control_and_traversal(self) -> None:
        for valid in ['scripts/ci/file.py', 'docs/a file.md']:
            self.assertEqual(w.repository_path(valid), valid)
        for invalid in [
            '', '/etc/passwd', '../x', 'a/../x', '.git/config', 'a/.git/config',
            '.GIT/config', 'a/.GIT/config', '.Git/hooks/pre-commit',
            'a//b', 'a\\b', 'a/',
        ]:
            with self.subTest(invalid=invalid), self.assertRaises(w.ContractRefusal):
                w.repository_path(invalid)

    def test_plan_authority_cannot_poison_the_module_constant(self) -> None:
        raw_request = review_request()
        request = w.normalize_request(raw_request)
        plan = w.plan(request)
        self.assertEqual(plan['authority'], w.AUTHORITY)
        self.assertIsNot(plan['authority'], w.AUTHORITY)
        self.assertIsNot(plan['source'], request['source'])
        plan['authority']['authorizes_merge'] = True
        self.assertFalse(w.AUTHORITY['authorizes_merge'])
        # normalize_receipt validates with `!= AUTHORITY`, so a poisoned
        # constant would silently widen every later check in this process.
        # It is not poisoned, so the widened authority is still refused.
        value = receipt(raw_request, {'kind': 'review', 'findings': [finding()]})
        value['authority'] = plan['authority']
        with self.assertRaisesRegex(w.ContractRefusal, 'unsupported authority'):
            w.normalize_receipt(value, request)

    def test_review_receipt_validates_finding_digest_and_zero_authority(self) -> None:
        request = w.decode_request(raw(review_request()))
        value = receipt(request, {'kind': 'review', 'findings': [finding()]})
        normalized = w.decode_receipt(raw(value), request)
        self.assertEqual(normalized['state'], 'completed')
        self.assertEqual(normalized['authority'], w.AUTHORITY)
        self.assertEqual(len(normalized['result']['findings']), 1)
        value['result']['findings'][0]['summary'] = 'drifted summary'
        with self.assertRaisesRegex(w.ContractRefusal, 'digest'):
            w.decode_receipt(raw(value), request)

    def test_repair_receipt_requires_patch_bounds_scope_and_all_profiles_passed(self) -> None:
        request_value = repair_request()
        request_value['verification'].append(
            {'id': 'cmux.macos.dev-check', 'generation': 1}
        )
        request = w.decode_request(raw(request_value))
        result = {
            'kind': 'repair',
            'result_source': {'commit': '5' * 40, 'tree': '6' * 40},
            'changes': [{
                'path': 'scripts/ci/cmux_workload_profile.py',
                'status': 'modified',
            }],
            'patch_sha256': A,
            'patch_bytes': 1024,
            'verification_results': [
                {
                    'profile': {'id': 'cmux.ci.guard', 'generation': 1},
                    'source': {'commit': '5' * 40, 'tree': '6' * 40},
                    'semantic_result': 'passed',
                    'result_sha256': D,
                },
                {
                    'profile': {'id': 'cmux.macos.dev-check', 'generation': 1},
                    'source': {'commit': '5' * 40, 'tree': '6' * 40},
                    'semantic_result': 'passed',
                    'result_sha256': C,
                },
            ],
            'working_copy': 'clean',
        }
        normalized = w.decode_receipt(raw(receipt(request, result)), request)
        self.assertEqual(normalized['result']['working_copy'], 'clean')
        self.assertEqual(normalized['result']['patch_bytes'], 1024)

        bad = copy.deepcopy(result)
        bad['changes'][0]['path'] = 'Sources/outside.swift'
        with self.assertRaisesRegex(w.ContractRefusal, 'outside'):
            w.decode_receipt(raw(receipt(request, bad)), request)

        bad = copy.deepcopy(result)
        bad['patch_bytes'] = repair_request()['mutation']['max_patch_bytes'] + 1
        with self.assertRaisesRegex(w.ContractRefusal, 'byte count'):
            w.decode_receipt(raw(receipt(request, bad)), request)

        bad = copy.deepcopy(result)
        bad['changes'][0]['status'] = 'added'
        with self.assertRaisesRegex(w.ContractRefusal, 'added a file'):
            w.decode_receipt(raw(receipt(request, bad)), request)

        bad = copy.deepcopy(result)
        bad['changes'][0]['status'] = 'deleted'
        with self.assertRaisesRegex(w.ContractRefusal, 'deleted a file'):
            w.decode_receipt(raw(receipt(request, bad)), request)

        bad = copy.deepcopy(result)
        bad['verification_results'][1]['source'] = copy.deepcopy(
            request['source']['head']
        )
        with self.assertRaisesRegex(w.ContractRefusal, 'exact resulting source'):
            w.decode_receipt(raw(receipt(request, bad)), request)

        bad = copy.deepcopy(result)
        bad['result_source']['commit'] = request['source']['head']['commit']
        with self.assertRaisesRegex(w.ContractRefusal, 'different exact source'):
            w.decode_receipt(raw(receipt(request, bad)), request)

        bad = copy.deepcopy(result)
        bad['result_source'] = copy.deepcopy(request['source']['head'])
        bad['verification_results'][0]['source'] = copy.deepcopy(
            request['source']['head']
        )
        with self.assertRaisesRegex(w.ContractRefusal, 'different exact source'):
            w.decode_receipt(raw(receipt(request, bad)), request)

        bad = copy.deepcopy(result)
        bad['verification_results'][0]['semantic_result'] = 'failed'
        with self.assertRaisesRegex(w.ContractRefusal, 'requires all'):
            w.decode_receipt(raw(receipt(request, bad)), request)

    def test_research_receipt_binds_cited_artifact_without_raw_answer(self) -> None:
        request = w.decode_request(raw(research_request()))
        result = {
            'kind': 'research',
            'result_class': 'answered',
            'artifact_sha256': A,
            'citation_manifest_sha256': B,
            'citation_count': 5,
            'evidence_source_count': 4,
        }
        normalized = w.decode_receipt(raw(receipt(request, result)), request)
        encoded = w.canonical_bytes(normalized)
        self.assertIn(b'citation_manifest_sha256', encoded)
        self.assertNotIn(b'raw_answer', encoded)
        self.assertNotIn(b'raw_output', encoded)

    def test_noncompleted_receipt_has_no_result(self) -> None:
        request = w.decode_request(raw(research_request()))
        value = receipt(request, None)
        value['state'] = 'ambiguous'
        normalized = w.decode_receipt(raw(value), request)
        self.assertIsNone(normalized['result'])
        value['result'] = {'kind': 'research'}
        with self.assertRaisesRegex(w.ContractRefusal, 'must not claim'):
            w.decode_receipt(raw(value), request)

    def test_nine_real_cmux_trial_fixtures_are_digest_bound_and_balanced(self) -> None:
        document = json.loads(FIXTURES.read_text(encoding='utf-8'))
        self.assertEqual(document['schema_version'], 1)
        self.assertEqual(document['fixture_class'], 'historical_exact_revision_trial')
        counts = {operation: 0 for operation in w.OPERATIONS}
        names: set[str] = set()
        for fixture in document['fixtures']:
            self.assertNotIn(fixture['name'], names)
            names.add(fixture['name'])
            self.assertTrue(fixture['work_ref'].startswith('github:manaflow-ai/cmux#'))
            self.assertEqual(
                fixture['request']['task_contract_sha256'],
                w.sha256(fixture['task_contract'].encode('utf-8')),
            )
            request = w.decode_request(raw(fixture['request']))
            counts[request['operation']] += 1
            self.assertEqual(request['source']['repository'], 'manaflow-ai/cmux')
            self.assertEqual(w.plan(request)['compute_workload']['family'], 'repository_agent_work.v1')
        self.assertEqual(counts, {'review': 3, 'repair': 3, 'research': 3})

    def test_cli_plan_and_receipt_validation_round_trip(self) -> None:
        request = w.decode_request(raw(research_request()))
        result = {
            'kind': 'research', 'result_class': 'answered',
            'artifact_sha256': A, 'citation_manifest_sha256': B,
            'citation_count': 1, 'evidence_source_count': 1,
        }
        with tempfile.TemporaryDirectory() as directory:
            request_path = Path(directory) / 'request.json'
            request_path.write_bytes(raw(request))
            planned = subprocess.run(
                [sys.executable, str(MODULE_PATH), 'plan'],
                input=raw(request), stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
            )
            self.assertEqual(planned.returncode, 0, planned.stderr.decode())
            self.assertEqual(json.loads(planned.stdout)['document_type'], w.PLAN_TYPE)
            validated = subprocess.run(
                [sys.executable, str(MODULE_PATH), 'validate-receipt', '--request', str(request_path)],
                input=raw(receipt(request, result)), stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
            )
            self.assertEqual(validated.returncode, 0, validated.stderr.decode())
            self.assertEqual(json.loads(validated.stdout)['state'], 'completed')


if __name__ == '__main__':
    unittest.main()
