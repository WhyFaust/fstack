import unittest

from check_routing import CASES, grade


class RoutingTests(unittest.TestCase):
    def observation(self, *items, final="", effort="low"):
        return {"status": "completed", "thread_id": "root", "final": final,
                "items": [{"threadId": "root", "item": item} for item in items],
                "agent_threads": {"worker": {"reasoningEffort": effort}}}

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
        result = {"final": case["result"]}
        self.assertEqual(grade(case, self.observation(skill, agent, **result), sources), [])
        self.assertTrue(grade(case, self.observation(skill, agent, effort="high", **result), sources))
        self.assertTrue(grade(case, self.observation(skill, agent, agent, **result), sources))
