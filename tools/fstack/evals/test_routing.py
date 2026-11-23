import json
from pathlib import Path
import tempfile
import unittest

from app_server import routing_evidence
from check_routing import CASES, grade


class RoutingTests(unittest.TestCase):
    def observation(self, *items, final="", effort="low", calls=(), supplied=(), replies=()):
        return {"status": "completed", "thread_id": "root", "final": final,
                "items": [{"threadId": "root", "item": item} for item in items],
                "agent_threads": {"worker": {"reasoningEffort": effort}},
                "routing": {"calls": list(calls), "skills_supplied": supplied, "replies": replies}}

    def command(self, output, code=0):
        return {"type": "commandExecution", "exitCode": code, "aggregatedOutput": output}

    def test_review_requires_successful_skill_read_before_mr_snapshot(self):
        case = CASES["local-review"]
        sources = {"watch-pr": "Инструкция проверки MR"}
        skill = self.command(sources["watch-pr"])
        mr = self.command(case["file"][1])
        result = {"final": case["result"]}
        self.assertEqual(grade(case, self.observation(skill, mr, **result), sources), [])
        self.assertTrue(grade(case, self.observation(mr, skill, **result), sources))
        failed_skill = self.command(sources["watch-pr"], code=1)
        self.assertTrue(grade(case, self.observation(failed_skill, mr, **result), sources))

    def test_citation_control_and_delegation_count_actual_actions(self):
        agent = {"type": "subAgentActivity", "kind": "started", "agentThreadId": "worker"}
        self.assertEqual(grade(CASES["quoted"], self.observation(), {}), [])
        self.assertTrue(grade(CASES["quoted"], self.observation(agent), {}))
        self.assertTrue(grade(CASES["quoted"], self.observation(self.command("", code=1)), {}))
        case = CASES["delegate"]
        sources = {"delegate": "Инструкция исполнителю"}
        skill = self.command(sources["delegate"])
        spawn = {"name": "spawn_agent", "fork_turns": "none", "reasoning_effort": "low"}
        result = {"final": case["result"], "calls": [spawn], "replies": [case["result"]]}
        self.assertEqual(grade(case, self.observation(skill, agent, **result), sources), [])
        self.assertTrue(grade(case, self.observation(skill, agent, effort="high", **result), sources))
        self.assertTrue(grade(case, self.observation(skill, agent, agent, **result), sources))
        self.assertTrue(grade(case, self.observation(skill, agent, **{**result, "replies": []}), sources))
        inherited = {**spawn, "fork_turns": "all"}
        self.assertTrue(grade(case, self.observation(skill, agent, **{**result, "calls": [inherited]}), sources))

    def test_explicit_invocation_and_unavailable_provider(self):
        explicit = CASES["explicit"]
        file = self.command(explicit["file"][1])
        only_how = self.observation(file, final=explicit["result"], supplied=("fstack:how",))
        self.assertEqual(grade(explicit, only_how, {}), [])
        other = self.command("Другой навык")
        extra = self.observation(file, other, final=explicit["result"], supplied=("fstack:how",))
        self.assertTrue(grade(explicit, extra, {"why": "Другой навык"}))

        unavailable = CASES["unavailable"]
        sources = {"delegate": "Инструкция исполнителю"}
        skill = self.command(sources["delegate"])
        final = f"Провайдер {unavailable['result']} недоступен."
        self.assertEqual(grade(unavailable, self.observation(skill, final=final), sources), [])
        launch = self.observation(skill, final=final, calls=[{"name": "spawn_agent"}])
        self.assertTrue(grade(unavailable, launch, sources))

    def test_routing_evidence_reads_request_and_delivery_from_rollout(self):
        thread = "c884232a-3476-42a4-941e-601661af30dc"
        items = [
            {"type": "message", "role": "user", "content": [{"text": "<skill>\n<name>fstack:how</name>"}]},
            {"type": "function_call", "name": "spawn_agent", "call_id": "s",
             "arguments": '{"fork_turns":"none","reasoning_effort":"low","message":"fixture"}'},
            {"type": "function_call", "name": "wait_agent", "call_id": "w"},
            {"type": "function_call_output", "call_id": "w", "output": "Полярная глина"},
        ]
        with tempfile.TemporaryDirectory() as temp:
            home = Path(temp)
            folder = home / "sessions/2026/11/23"
            folder.mkdir(parents=True)
            (folder / f"rollout-fixture-{thread}.jsonl").write_text(
                "".join(json.dumps({"type": "response_item", "payload": item}) + "\n" for item in items),
                encoding="utf-8")
            evidence = routing_evidence(home, thread)
        self.assertEqual(evidence["skills_supplied"], ["fstack:how"])
        self.assertEqual(evidence["calls"][0], {"name": "spawn_agent", "fork_turns": "none",
                                                "reasoning_effort": "low"})
        self.assertEqual(evidence["replies"], ["Полярная глина"])
