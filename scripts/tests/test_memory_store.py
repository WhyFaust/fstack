import os
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "skills" / "use-memory" / "scripts"))
import memory_store
from memory_store import Store, StoreError


class MemoryStoreTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.temp = Path(temporary.name).resolve()
        self.repo = self.temp / "repo"
        self.repo.mkdir()
        self.vault = self.temp / "vault"
        self.vault.mkdir()
        (self.vault / ".obsidian").mkdir()
        self.store = Store(self.temp / "profile")
        self.candidate = {
            "kind": "decision",
            "title": "Порог ожидания",
            "body": "Использовать три секунды для чтения конфигурации.",
        }

    def setup_store(self, mode="auto"):
        return self.store.setup(self.vault, self.repo, "project", mode)

    def entry_path(self, binding, status, entry_id):
        return binding.root / "Projects" / binding.project / status / f"{entry_id}.md"

    def git(self, *args):
        environment = {key: value for key, value in os.environ.items() if not key.upper().startswith("GIT_")}
        environment.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        return subprocess.run(
            ["git", "-C", str(self.repo), *args], env=environment, check=True,
            capture_output=True, text=True, encoding="utf-8", timeout=10,
        ).stdout.strip()

    def test_git_worktree_binding_and_forged_pointer(self):
        self.git("init")
        self.git("-c", "user.name=Memory Test", "-c", "user.email=test@example.invalid",
                 "commit", "--allow-empty", "-m", "init")
        linked = self.temp / "linked worktree"
        self.git("worktree", "add", "--detach", str(linked))
        nested = linked / "nested"
        nested.mkdir()
        binding = self.store.setup(self.vault, nested, "project", "auto")
        self.assertEqual(self.store.resolve(self.repo), binding)
        self.assertEqual(self.store.resolve(nested), binding)
        self.assertEqual(self.store.setup(self.vault, self.repo, "project", "auto"), binding)
        forged_pointers = (
            (linked / "unregistered", (linked / ".git").read_text(encoding="utf-8")),
            (self.temp / "unregistered", f"gitdir: {(self.repo / '.git').as_posix()}\n"),
        )
        for directory, pointer in forged_pointers:
            directory.mkdir()
            (directory / ".git").write_text(pointer, encoding="utf-8")
            with self.subTest(directory=directory), self.assertRaises(StoreError):
                self.store.resolve(directory)
        outside = self.temp / "outside"
        outside.mkdir()
        with patch.dict(os.environ, {"GIT_DIR": str(self.repo / ".git"), "GIT_WORK_TREE": str(outside)}):
            self.assertIsNone(self.store.resolve(outside))
        self.assertTrue(self.store.disable(linked))
        self.assertIsNone(self.store.resolve(self.repo))

    def test_git_lookup_ignores_worktree_executables_and_relative_path(self):
        self.git("init")
        fake = self.repo / ("git.exe" if os.name == "nt" else "git")
        fake.write_text("this is not Git", encoding="utf-8")
        if os.name != "nt":
            fake.chmod(0o700)
        untrusted = ["", ".", str(self.repo)]
        path = os.pathsep.join([*untrusted, os.environ.get("PATH", "")])
        with patch.dict(os.environ, {"PATH": path}):
            with patch("memory_store.subprocess.run", wraps=subprocess.run) as run:
                self.assertIsNone(self.store.resolve(self.repo))
                self.assertEqual(run.call_count, 2)
                for call in run.call_args_list:
                    chosen = Path(call.kwargs["executable"])
                    self.assertTrue(chosen.is_absolute())
                    self.assertFalse(chosen.is_relative_to(self.repo))
                    self.assertEqual(Path(call.args[0][0]), chosen)
        with patch.dict(os.environ, {"PATH": os.pathsep.join(untrusted)}):
            with self.assertRaises(StoreError):
                self.store.resolve(self.repo)
            directory = self.temp / "plain"
            directory.mkdir()
            binding = self.store.setup(self.vault, directory, "plain", "auto")
            self.assertEqual(self.store.resolve(directory), binding)

    def test_inbox_promotion_preserves_manual_edit_and_deduplicates_across_profiles(self):
        binding = self.setup_store("inbox")
        entry = self.store.save(binding, self.candidate, approved=True, turn_id="original-turn")
        self.assertEqual(entry["status"], "inbox")
        self.assertEqual(self.store.search(binding, "порог"), [])
        pending = self.entry_path(binding, "Records", entry["id"])
        edited = pending.read_text(encoding="utf-8").replace('"Порог ожидания"', "Новый порог")
        pending.write_text(edited, encoding="utf-8")
        approved = self.store.approve(binding, entry["id"])
        self.assertEqual(approved["title"], "Новый порог")
        self.assertEqual(approved["turn_id"], "original-turn")
        self.assertEqual(self.store.approve(binding, entry["id"]), approved)
        self.assertEqual(self.store.inbox(binding), [])
        self.assertEqual(pending.read_text(encoding="utf-8"), edited)
        marker = self.entry_path(binding, "Approved", entry["id"])
        self.assertIn("fstack_approved: 1", marker.read_text(encoding="utf-8"))
        self.assertNotIn(approved["body"], marker.read_text(encoding="utf-8"))
        other_store = Store(self.temp / "other-profile")
        other_binding = other_store.setup(self.vault, self.repo, "project", "auto")
        self.assertEqual(
            other_store.save(other_binding, self.candidate, approved=True, turn_id="other-turn"),
            approved,
        )
        self.assertEqual(other_store.search(other_binding, "ПОРОГ секунды"), [approved])

    def test_supersedes_and_tombstone_hide_stale_sync_copy(self):
        binding = self.setup_store()
        previous = self.store.save(binding, self.candidate, approved=True)
        changed = dict(self.candidate, body="Использовать пять секунд.", supersedes=previous["id"])
        correction = self.store.save(binding, changed, approved=True)
        with self.assertRaises(StoreError):
            self.store.get(binding, previous["id"])
        self.assertEqual(self.store.save(binding, changed, approved=True), correction)
        path = self.entry_path(binding, "Records", correction["id"])
        stale = path.read_bytes()
        self.assertEqual(self.store.forget(binding, correction["id"])["status"], "forgotten")
        tombstone = self.entry_path(binding, "Forgotten", correction["id"])
        self.assertNotIn(correction["body"], tombstone.read_text(encoding="utf-8"))
        self.assertFalse(path.exists())
        path.write_bytes(stale)
        self.assertEqual(self.store.search(binding, "порог"), [])
        with self.assertRaises(StoreError):
            self.store.save(binding, changed, approved=True)
        self.store.forget(binding, correction["id"])
        self.assertFalse(path.exists())
        old_path = self.entry_path(binding, "Records", previous["id"])
        edited = old_path.read_text(encoding="utf-8").replace(self.candidate["body"], "token = sample-private-value")
        old_path.write_text(edited, encoding="utf-8")
        self.store.forget(binding, previous["id"])
        self.assertFalse(old_path.exists())

    def test_concurrent_promotion_and_saving_publish_one_record(self):
        binding = self.setup_store()
        draft = self.store.save(binding, self.candidate, approved=False)

        def publish(index):
            if index % 2:
                return self.store.approve(binding, draft["id"])
            return self.store.save(binding, self.candidate, approved=True)

        with ThreadPoolExecutor(max_workers=4) as pool:
            entries = list(pool.map(publish, range(8)))
        self.assertTrue(all(entry == entries[0] for entry in entries))
        self.assertEqual(self.store.inbox(binding), [])
        self.assertEqual(self.store.search(binding, "порог"), [entries[0]])

    def test_approval_write_failure_leaves_editable_inbox_for_retry(self):
        binding = self.setup_store()
        write_text = memory_store._write_text

        def fail_marker(path, text):
            if path.parent.name == "Approved":
                raise StoreError("interrupted before publishing approval")
            return write_text(path, text)

        with patch("memory_store._write_text", side_effect=fail_marker), self.assertRaises(StoreError):
            self.store.save(binding, self.candidate, approved=True)
        draft, = self.store.inbox(binding)
        self.assertEqual(self.store.search(binding, "порог"), [])
        record = self.entry_path(binding, "Records", draft["id"])

        def edit_as_approval_is_published(path, text):
            if path.parent.name == "Approved":
                revised = record.read_text(encoding="utf-8").replace("три секунды", "пять секунд")
                record.write_text(revised, encoding="utf-8")
            return write_text(path, text)

        with patch("memory_store._write_text", side_effect=edit_as_approval_is_published):
            approved = self.store.save(binding, self.candidate, approved=True)
        self.assertEqual(approved["id"], draft["id"])
        self.assertIn("пять секунд", approved["body"])
        self.assertIn("пять секунд", record.read_text(encoding="utf-8"))
        self.assertEqual(self.store.get(binding, approved["id"]), approved)

    def test_forget_interleaved_with_approval_keeps_predecessor_hidden(self):
        binding = self.setup_store()
        predecessor = self.store.save(binding, self.candidate, approved=True)
        correction = dict(self.candidate, body="Использовать пять секунд.", supersedes=predecessor["id"])
        draft = self.store.save(binding, correction, approved=False)
        record = self.entry_path(binding, "Records", draft["id"])
        original = record.read_bytes()
        write_text = memory_store._write_text

        def forget_before_approval_is_published(path, text):
            if path.parent.name == "Approved":
                self.store.forget(binding, draft["id"])
            return write_text(path, text)

        with patch("memory_store._write_text", side_effect=forget_before_approval_is_published):
            with self.assertRaises(StoreError):
                self.store.approve(binding, draft["id"])
        self.assertFalse(record.exists())
        self.assertEqual(self.store.search(binding, "порог"), [])
        record.write_bytes(original)
        self.assertEqual(self.store.search(binding, "порог"), [])
        self.assertEqual(self.store.inbox(binding), [])

    def test_search_fallback_stays_within_the_connected_project(self):
        binding = self.setup_store()
        entry = self.store.save(binding, self.candidate, approved=True)
        other_repo = self.temp / "other-repo"
        other_repo.mkdir()
        other = self.store.setup(self.vault, other_repo, "other", "auto")
        self.store.save(other, self.candidate, approved=True)
        bad_neighbor = self.entry_path(other, "Records", entry["id"])
        bad_neighbor.write_bytes(b"\xff\xff")
        nested = self.repo / "not-connected"
        nested.mkdir()
        self.assertIsNone(self.store.resolve(nested))
        no_fts = Mock()
        no_fts.execute.side_effect = sqlite3.OperationalError("no such module: fts5")
        with patch("memory_store.sqlite3.connect", return_value=no_fts):
            self.assertEqual(self.store.search(binding, "ПОРОГ секунды"), [entry])
            self.assertEqual(self.store.search(binding, "секунда"), [])
        with self.assertRaises(StoreError):
            self.store.search(other, "порог")

    def test_search_natural_query_recalls_ranked_partial_matches(self):
        binding = self.setup_store()
        entry = self.store.save(binding, self.candidate, approved=True)
        weaker_match = dict(self.candidate, title="Размещение", body="Для архива задан порог объёма.")
        self.store.save(binding, weaker_match, approved=True)
        query = "Какой порог ожидания мы согласовали для этого проекта?"
        self.assertEqual(self.store.search(binding, query, limit=1), [entry])
        no_fts = Mock()
        no_fts.execute.side_effect = sqlite3.OperationalError("no such module: fts5")
        with patch("memory_store.sqlite3.connect", return_value=no_fts):
            self.assertEqual(self.store.search(binding, query, limit=1), [entry])
            self.assertEqual(self.store.search(binding, "мы и для этого"), [])

    def test_unsafe_data_and_path_are_rejected(self):
        occupied = self.vault / "Codex Memory"
        occupied.mkdir()
        private = occupied / "private.md"
        private.write_text("unmanaged", encoding="utf-8")
        with self.assertRaises(StoreError):
            self.setup_store()
        self.assertEqual(private.read_text(encoding="utf-8"), "unmanaged")
        private.unlink()
        binding = self.setup_store()
        for candidate in (
            dict(self.candidate, body="api_key = sample-private-value"),
            dict(self.candidate, body="![[another project/file]]"),
            dict(self.candidate, body="x" * 4001),
            dict(self.candidate, supersedes="a" * 64),
            dict(self.candidate, transcript="unrequested input"),
        ):
            with self.subTest(candidate=candidate.keys()), self.assertRaises(StoreError):
                self.store.save(binding, candidate, approved=True)
        with self.assertRaises(StoreError):
            self.store.forget(binding, "../../private")
        self.assertEqual(self.store.search(binding, "порог"), [])

    def test_linked_memory_folder_is_rejected_before_use(self):
        binding = self.setup_store()
        folder = binding.root / "Projects" / "project" / "Records"
        folder.rmdir()
        outside = self.temp / "outside"
        outside.mkdir()
        if sys.platform == "win32":
            subprocess.run(["cmd", "/c", "mklink", "/J", str(folder), str(outside)],
                           check=True, capture_output=True)
        else:
            folder.symlink_to(outside, target_is_directory=True)
        try:
            with self.assertRaises(StoreError):
                self.store.search(binding, "порог")
            with self.assertRaises(StoreError):
                self.store.save(binding, self.candidate, approved=True)
            self.assertEqual(list(outside.iterdir()), [])
        finally:
            if sys.platform == "win32":
                folder.rmdir()
            else:
                folder.unlink()
