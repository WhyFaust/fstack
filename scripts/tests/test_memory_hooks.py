import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from contextlib import chdir, closing
from pathlib import Path
from threading import Event
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "skills" / "use-memory" / "scripts" / "memory.py"
sys.path.insert(0, str(SCRIPT.parent))
import hook_spool
import memory
TOKEN = re.compile(r"stage --turn ([0-9a-f]{64}\.[0-9a-f]{32})")
CANDIDATE = {"kind": "decision", "title": "Heliotrope convention", "body": "The heliotrope marker is retained."}
REMEMBER = {"action": "remember", "candidate": CANDIDATE}


class MemoryHookTests(unittest.TestCase):
    def setUp(self):
        workspace = self.enterContext(tempfile.TemporaryDirectory(prefix="fstack hooks "))
        self.root = Path(workspace).resolve()
        cache = str(self.root / "cache")
        self.enterContext(patch.dict(os.environ, {"LOCALAPPDATA": cache, "XDG_CACHE_HOME": cache}))
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.vault = self.root / "vault"
        (self.vault / ".obsidian").mkdir(parents=True)
        self.state = self.root / "state"

    def git_init(self, cwd):
        subprocess.run(["git", "init", "--quiet"], cwd=cwd, check=True, capture_output=True, timeout=10)

    def cli(self, *args, payload=None, cwd=None, ok=True):
        result = subprocess.run(
            [sys.executable, "-B", str(SCRIPT), "--state-dir", str(self.state), *args],
            cwd=cwd or self.repo,
            input=json.dumps(payload).encode("utf-8") if payload is not None else b"",
            capture_output=True,
            timeout=10,
        )
        if not ok:
            self.assertEqual(result.returncode, 1, result.stderr.decode())
            return result
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        return json.loads(result.stdout)

    def init(self, mode="auto"):
        return self.cli(
            "init", "--vault", str(self.vault), "--repo", str(self.repo), "--project", "pilot", "--mode", mode,
        )

    def hook(self, event, turn="turn-1", *, cwd=None, **extra):
        cwd = cwd or self.repo
        payload = {
            "hook_event_name": event,
            "session_id": "00000000-0000-4000-a000-000000000001",
            "cwd": str(cwd),
            "transcript_path": str(self.root / "never-read.jsonl"),
            "permission_mode": "default",
            "model": "example-model",
            **extra,
        }
        if event != "SessionStart":
            payload["turn_id"] = turn
        if event == "Stop":
            payload.setdefault("stop_hook_active", False)
        return self.cli("hook", event, payload=payload, cwd=cwd)

    def context(self, prompt="heliotrope", turn="turn-1", **extra):
        return self.hook("UserPromptSubmit", turn, prompt=prompt, **extra)["hookSpecificOutput"]["additionalContext"]

    def token(self, turn="turn-1"):
        return TOKEN.search(self.context(turn=turn))[1]

    def stage_command(self, token, action):
        encoded = self.cli("pack", payload=action)
        invoke = "& " if os.name == "nt" else ""
        python = Path(sys.executable).resolve()
        return encoded, f'{invoke}"{python}" -E -s -B "{SCRIPT}" stage --turn {token} --payload {encoded}'

    def post(self, command, turn="turn-1", **overrides):
        return self.hook(
            "PostToolUse", turn,
            **{
                "tool_use_id": "call-stage",
                "tool_name": "Bash",
                "tool_input": {"command": command},
                "tool_response": '{"staged": true}\n',
                **overrides,
            },
        )

    def stage(self, token, action, turn="turn-1"):
        encoded, command = self.stage_command(token, action)
        self.assertEqual(self.cli("stage", "--turn", token, "--payload", encoded), {"staged": True})
        self.assertEqual(self.post(command, turn), {})
        return command

    def test_no_binding_or_plan_can_issue_a_token(self):
        self.assertEqual(self.hook("UserPromptSubmit", prompt="heliotrope"), {})
        self.assertEqual(self.hook("Stop"), {})
        self.assertFalse((self.vault / "Codex Memory").exists())
        self.init()
        self.assertEqual(self.hook("UserPromptSubmit", prompt="heliotrope", permission_mode="plan"), {})
        self.assertEqual(self.hook("UserPromptSubmit", prompt="heliotrope", collaboration_mode="plan"), {})
        self.assertFalse((self.state / "pending.sqlite3").exists())

    def test_submit_only_injects_scoped_ids_and_does_not_create_spool(self):
        self.git_init(self.repo)
        self.init()
        self.assertIn("pilot (auto)", self.hook("SessionStart")["hookSpecificOutput"]["additionalContext"])
        entry = self.cli("remember", payload=CANDIDATE)
        context = self.context()
        self.assertIn(entry["id"], context)
        self.assertNotIn(CANDIDATE["title"], context)
        self.assertNotIn(CANDIDATE["body"], context)
        self.assertNotIn("PRIVATEPROMPT", self.context("PRIVATEPROMPT"))
        self.assertFalse((self.repo / ".fstack-memory-stage").exists())
        sibling = self.root / "outside"
        sibling.mkdir()
        self.assertEqual(self.hook("UserPromptSubmit", cwd=sibling, prompt="heliotrope"), {})
        self.assertEqual(self.hook("UserPromptSubmit", prompt="heliotrope", cwd=self.repo, hook_event_name="Stop"), {})

    def test_stop_only_commits_staged_current_turn_and_is_repeatable(self):
        self.init()
        token = self.token()
        command = self.stage(token, REMEMBER)
        self.assertEqual(self.post(command), {})
        self.assertEqual(self.cli("search", "heliotrope"), [])
        for event in ({"stop_hook_active": True}, {"session_id": "another-session"}):
            with self.subTest(event=event):
                self.assertEqual(self.hook("Stop", **event), {})
                self.assertEqual(self.cli("search", "heliotrope"), [])
        self.assertEqual(self.hook("Stop", "other-turn"), {})
        self.assertEqual(self.cli("search", "heliotrope"), [])
        self.assertEqual(self.hook("Stop", last_assistant_message="PRIVATEASSISTANT"), {})
        records = self.cli("search", "heliotrope")
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]["body"], CANDIDATE["body"])
        self.assertEqual(records[0]["turn_id"], "turn-1")
        self.assertEqual(self.hook("Stop"), {})
        encoded, _ = self.stage_command(token, REMEMBER)
        self.cli("stage", "--turn", token, "--payload", encoded, ok=False)
        self.assertNotIn(CANDIDATE["body"].encode(), (self.state / "pending.sqlite3").read_bytes())
        self.assertEqual(self.cli("search", "heliotrope"), records)

    def test_rejects_wrong_token_and_unsanctioned_data_fields(self):
        self.init()
        token = self.token()
        encoded, _ = self.stage_command(token, REMEMBER)
        suffix = "0" if token[-1] != "0" else "1"
        self.cli("stage", "--turn", token[:-1] + suffix, "--payload", encoded, ok=False)
        self.cli("stage", "--turn", "../turn", "--payload", encoded, ok=False)
        self.cli("pack", payload={**REMEMBER, "path": str(self.root)}, ok=False)
        self.cli("pack", payload={**REMEMBER, "candidate": dict(CANDIDATE, body="x" * 40_000)}, ok=False)
        self.assertEqual(self.hook("Stop"), {})
        self.assertEqual(self.cli("inbox"), [])

    def test_stage_respects_inbox_mode_approval_and_forgetting(self):
        self.init(mode="inbox")
        self.stage(self.token(), REMEMBER)
        self.assertEqual(self.hook("Stop"), {})
        self.assertEqual(self.cli("search", "heliotrope"), [])
        entry = self.cli("inbox")[0]
        self.stage(self.token("turn-2"), {"action": "approve", "id": entry["id"]}, "turn-2")
        self.assertEqual(self.hook("Stop", "turn-2"), {})
        self.assertEqual(self.cli("inbox"), [])
        self.assertEqual(self.cli("show", entry["id"])["status"], "memory")
        self.stage(self.token("turn-3"), {"action": "forget", "id": entry["id"]}, "turn-3")
        self.assertEqual(self.hook("Stop", "turn-3"), {})
        self.assertEqual(self.cli("search", "heliotrope"), [])

    def test_stop_revalidates_binding_and_ignores_an_unstaged_assistant_message(self):
        self.init()
        self.assertEqual(self.hook("Stop", last_assistant_message=json.dumps(CANDIDATE)), {})
        self.assertEqual(self.cli("search", "heliotrope"), [])
        self.stage(self.token(), {"action": "draft", "candidate": CANDIDATE})
        self.init(mode="inbox")
        self.assertEqual(self.hook("Stop"), {})
        self.assertEqual(self.cli("inbox"), [])
        self.assertEqual(self.cli("disable"), {"disabled": True})
        self.assertEqual(self.cli("status"), {"enabled": False})

    def test_workspace_file_and_read_only_stage_cannot_supply_an_action(self):
        self.init()
        token = self.token()
        entry = self.cli("remember", payload=CANDIDATE)
        action = {"action": "forget", "id": entry["id"]}
        db = self.state / "pending.sqlite3"
        snapshot = db.read_bytes()
        encoded, _ = self.stage_command(token, action)
        self.assertEqual(self.cli("stage", "--turn", token, "--payload", encoded), {"staged": True})
        self.assertEqual(db.read_bytes(), snapshot)
        forged = self.repo / ".fstack-memory-stage" / f'{token.split(".")[0]}.json'
        forged.parent.mkdir()
        data = json.dumps({"token": token, "stage": action})
        forged.write_text(data, encoding="utf-8")
        self.assertEqual(self.hook("Stop"), {})
        self.assertEqual(self.cli("show", entry["id"]), entry)
        self.assertEqual(forged.read_text(encoding="utf-8"), data)

    def test_steering_invalidates_previous_action_and_late_hook(self):
        self.init()
        token = self.token()
        old_command = self.stage(token, REMEMBER)
        new_token = self.token()
        self.assertNotEqual(token, new_token)
        self.assertEqual(self.post(old_command), {})
        self.assertEqual(self.hook("Stop"), {})
        self.assertEqual(self.cli("search", "heliotrope"), [])
        token = self.token("turn-2")
        self.stage(token, REMEMBER, "turn-2")
        self.assertEqual(self.hook("UserPromptSubmit", "turn-2", permission_mode="plan"), {})
        self.assertEqual(self.hook("Stop", "turn-2"), {})
        self.assertEqual(self.cli("search", "heliotrope"), [])

    def test_steering_revokes_the_action_even_when_database_write_is_locked(self):
        self.init()
        self.stage(self.token(), REMEMBER)
        with closing(sqlite3.connect(self.state / "pending.sqlite3")) as locked, locked:
            locked.execute("BEGIN IMMEDIATE")
            self.assertIn("systemMessage", self.hook("UserPromptSubmit", prompt="Withdraw"))
        self.assertEqual(self.hook("Stop"), {})
        self.assertEqual(self.cli("search", "heliotrope"), [])
        self.assertNotIn(CANDIDATE["body"].encode(), (self.state / "pending.sqlite3").read_bytes())

    def test_steering_revokes_forget_even_when_generation_replacement_fails(self):
        self.init()
        entry = self.cli("remember", payload=CANDIDATE)
        token = self.token()
        action = {"action": "forget", "id": entry["id"]}
        command = self.stage(token, action)
        key, nonce = token.split(".")
        generation = self.state / "pending-generations" / key
        write_text = hook_spool._write_text

        def locked_generation(path, text, *, replace=False):
            if path == generation and replace:
                raise PermissionError("Generation file is open without delete sharing")
            return write_text(path, text, replace=replace)

        payload = {
            "hook_event_name": "UserPromptSubmit", "cwd": str(self.repo),
            "session_id": "00000000-0000-4000-a000-000000000001", "turn_id": "turn-1", "prompt": "Withdraw",
        }
        if os.name == "nt":
            with generation.open("rb"):
                result = self.hook("UserPromptSubmit", prompt="Withdraw")
            self.assertNotIn("decision", result)
            self.assertIn("systemMessage", result)
        else:
            with chdir(self.repo), patch.object(hook_spool, "_write_text", locked_generation):
                with self.assertRaises(PermissionError):
                    memory.hook(memory.Store(self.state), "UserPromptSubmit", payload)
        self.assertEqual(generation.read_text(encoding="utf-8"), nonce)
        self.assertTrue((self.state / "pending-revocations" / token).is_file())
        encoded, _ = self.stage_command(token, action)
        self.cli("stage", "--turn", token, "--payload", encoded, ok=False)
        self.assertEqual(self.post(command), {})
        self.assertEqual(self.hook("Stop"), {})
        self.assertEqual(self.hook("Stop"), {})
        self.assertEqual(self.cli("show", entry["id"]), entry)

    def test_steering_blocks_when_the_revocation_marker_cannot_be_written(self):
        self.init()
        self.stage(self.token(), REMEMBER)
        payload = {
            "hook_event_name": "UserPromptSubmit", "cwd": str(self.repo),
            "session_id": "00000000-0000-4000-a000-000000000001", "turn_id": "turn-1", "prompt": "Withdraw",
        }
        with chdir(self.repo), patch.object(hook_spool, "_write_text", side_effect=PermissionError):
            result = memory.hook(memory.Store(self.state), "UserPromptSubmit", payload)
        self.assertEqual(result["decision"], "block")
        self.assertTrue(result["reason"])

    def test_slow_vault_save_does_not_block_steering_in_another_session(self):
        self.init()
        self.stage(self.token(), REMEMBER)
        first_session = "first-session"
        first_token = TOKEN.search(self.context(session_id=first_session))[1]
        _, command = self.stage_command(first_token, REMEMBER)
        self.assertEqual(self.post(command, session_id=first_session), {})
        saving, resume = Event(), Event()
        store = memory.Store(self.state)
        save = store.save

        def slow_save(*args, **kwargs):
            saving.set()
            if not resume.wait(10):
                raise RuntimeError("Vault test did not release the save")
            return save(*args, **kwargs)

        payload = {
            "hook_event_name": "Stop", "cwd": str(self.repo), "session_id": first_session,
            "turn_id": "turn-1", "stop_hook_active": False,
        }
        with chdir(self.repo), patch.object(store, "save", slow_save), ThreadPoolExecutor(1) as pool:
            result = pool.submit(memory.hook, store, "Stop", payload)
            try:
                self.assertTrue(saving.wait(5))
                self.assertIn("hookSpecificOutput", self.hook("UserPromptSubmit", prompt="Withdraw"))
                self.assertEqual(self.hook("Stop"), {})
                self.assertEqual(self.cli("search", "heliotrope"), [])
            finally:
                resume.set()
            self.assertEqual(result.result(timeout=10), {})
        self.assertEqual(len(self.cli("search", "heliotrope")), 1)

    def test_post_tool_use_only_accepts_the_exact_shell_invocation(self):
        self.init()
        token = self.token()
        _, command = self.stage_command(token, REMEMBER)
        for changed in (
            {"tool_name": "apply_patch"},
            {"agent_id": "another-agent"},
            {"tool_response": "error"},
            {"tool_input": {"command": f"echo {command}"}},
            {"tool_input": {"command": f"{command}; other"}},
            {"tool_input": {"command": command, "another": "argument"}},
            {"tool_input": {"command": command.replace(str(SCRIPT), str(self.repo / "memory.py"))}},
            {"tool_input": {"command": command.replace("--payload", "--PAYLOAD")}},
        ):
            with self.subTest(changed=changed):
                self.assertEqual(self.post(command, **changed), {})
        self.assertEqual(self.hook("Stop"), {})
        self.assertEqual(self.cli("search", "heliotrope"), [])

    def test_rejects_pending_state_inside_the_workspace(self):
        self.state = self.repo / "fake-codex-home"
        self.cli("init", "--vault", str(self.vault), "--repo", str(self.repo), "--project", "pilot", ok=False)
        self.assertIn("systemMessage", self.hook("UserPromptSubmit", prompt="heliotrope"))
        self.assertFalse((self.state / "pending.sqlite3").exists())

    def test_init_rejects_a_python_binary_inside_the_worktree(self):
        self.git_init(self.repo)
        nested = self.repo / "nested"
        nested.mkdir()
        executable = self.repo / ".venv" / "python"
        executable.parent.mkdir()
        executable.write_bytes(b"not a trusted Python")
        executable.chmod(0o755)
        with patch.object(memory.sys, "executable", str(executable)):
            with self.assertRaises(memory.StoreError):
                memory.pinned_python(nested, nested)

    def test_pending_state_rejects_a_link_to_an_outside_directory(self):
        self.init()
        outside = self.root / "outside"
        outside.mkdir()
        link = self.state / "pending.sqlite3"
        if os.name == "nt":
            subprocess.run(["cmd", "/c", "mklink", "/J", str(link), str(outside)],
                           check=True, capture_output=True, timeout=10)
            self.addCleanup(link.rmdir)
        else:
            link.symlink_to(outside, target_is_directory=True)
        self.assertIn("systemMessage", self.hook("UserPromptSubmit", prompt="heliotrope"))
        self.assertEqual(list(outside.iterdir()), [])

    def test_manifest_command_expands_an_installed_plugin_root_with_spaces(self):
        manifest = json.loads((ROOT / "hooks" / "hooks.json").read_text(encoding="utf-8"))
        plugin = self.root / "plugin with spaces"
        script = plugin / SCRIPT.relative_to(ROOT)
        script.parent.mkdir(parents=True)
        script.write_text(
            "import json, sys\n"
            "print(json.dumps({'args': sys.argv[1:], 'input': json.load(sys.stdin.buffer)}))\n",
            encoding="utf-8",
        )
        (plugin / "hooks").mkdir()
        for launcher in ("memory.sh", "memory.ps1"):
            shutil.copyfile(ROOT / "hooks" / launcher, plugin / "hooks" / launcher)
        environment = os.environ.copy()
        environment["PLUGIN_ROOT"] = str(plugin)
        profile = self.root / "isolated profile"
        environment["CODEX_HOME"] = str(profile)
        pin = profile / "fstack" / "memory" / "python-executable"
        pin.parent.mkdir(parents=True)
        pin.write_text(f"{Path(sys.executable).resolve()}\n", encoding="utf-8")
        self.git_init(self.repo)
        nested = self.repo / "nested"
        nested.mkdir()
        fake_bin = self.repo / ".venv" / ("Scripts" if os.name == "nt" else "bin")
        fake_bin.mkdir(parents=True)
        fake = fake_bin / ("python.exe" if os.name == "nt" else "python3")
        fake.write_text("not a Python executable", encoding="utf-8")
        fake.chmod(0o755)
        path = [str(fake_bin), str(Path(sys.executable).parent), environment["PATH"]]
        if os.name == "nt":
            shells = [shutil.which("powershell.exe")]
            self.assertIsNotNone(shells[0])
            if pwsh := shutil.which("pwsh.exe"):
                shells.append(pwsh)
            shells = [[shell, "-NoProfile", "-ExecutionPolicy", "Restricted", "-NonInteractive", "-Command"]
                      for shell in shells]
        else:
            shells = [["/bin/sh", "-c"]]
        environment["PATH"] = os.pathsep.join(path)
        for event, groups in manifest["hooks"].items():
            handler = groups[0]["hooks"][0]
            command = handler["commandWindows" if os.name == "nt" else "command"]
            payload = {"hook_event_name": event, "example": "пример"}
            for shell in shells:
                with self.subTest(event=event, shell=shell[0]):
                    result = subprocess.run(
                        [*shell, command],
                        cwd=nested,
                        env=environment,
                        input=json.dumps(payload, ensure_ascii=False).encode("utf-8"),
                        capture_output=True,
                        timeout=10,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))
                    self.assertEqual(json.loads(result.stdout), {"args": ["hook", event], "input": payload})
        pin.write_text(f"{fake}\n", encoding="utf-8")
        for shell in shells:
            with self.subTest(unsafe_pin=shell[0]):
                result = subprocess.run(
                    [*shell, command], cwd=nested, env=environment, input=b"{}", capture_output=True, timeout=10,
                )
                self.assertEqual((result.returncode, result.stdout, result.stderr), (0, b"", b""))
        unsafe_profile = self.repo / "unsafe profile"
        unsafe_pin = unsafe_profile / "fstack" / "memory" / "python-executable"
        unsafe_pin.parent.mkdir(parents=True)
        unsafe_pin.write_text(f"{Path(sys.executable).resolve()}\n", encoding="utf-8")
        for shell in shells:
            with self.subTest(unsafe_profile=shell[0]):
                result = subprocess.run(
                    [*shell, command], cwd=nested, env={**environment, "CODEX_HOME": str(unsafe_profile)},
                    input=b"{}", capture_output=True, timeout=10,
                )
                self.assertEqual((result.returncode, result.stdout, result.stderr), (0, b"", b""))
