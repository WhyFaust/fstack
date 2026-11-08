import errno
import os
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from threading import Event
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "skills" / "use-memory" / "scripts"))
import memory_store
from memory_store import Store, StoreError


class MemoryStoreTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.temp = Path(temporary.name).resolve()
        locks = patch("memory_store._lock_directory", return_value=self.temp / "locks")
        locks.start()
        self.addCleanup(locks.stop)
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

    def legacy_approval(self, binding, entry_id, supersedes=None):
        path = self.entry_path(binding, "Approved", entry_id)
        path.parent.mkdir(exist_ok=True)
        predecessor = f'supersedes: "{supersedes}"\n' if supersedes else ""
        path.write_text(f'---\nfstack_approved: 1\nid: "{entry_id}"\n{predecessor}---\n', encoding="utf-8")
        return path

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
        original = pending.read_text(encoding="utf-8")
        self.assertIn('project_link: "[[Codex Memory/Projects/project/Project|project]]"', original)
        self.assertNotIn("fstack_status", original)
        edited = original.replace('"Порог ожидания"', "Новый порог").replace("три секунды", "пять секунд")
        pending.write_text(edited, encoding="utf-8")
        approved = self.store.approve(binding, entry["id"])
        self.assertEqual(approved["title"], "Новый порог")
        self.assertEqual(approved["turn_id"], "original-turn")
        self.assertEqual(set(approved), set(entry))
        self.assertEqual(self.store.approve(binding, entry["id"]), approved)
        self.assertEqual(self.store.inbox(binding), [])
        self.assertEqual(pending.read_text(encoding="utf-8").replace('fstack_status: "approved"\n', ""), edited)
        marker = self.entry_path(binding, "Approved", entry["id"])
        self.assertFalse(marker.parent.exists())
        other_store = Store(self.temp / "other-profile")
        other_binding = other_store.setup(self.vault, self.repo, "project", "auto")
        self.assertEqual(
            other_store.save(other_binding, self.candidate, approved=True, turn_id="other-turn"),
            approved,
        )
        self.assertEqual(other_store.search(other_binding, "ПОРОГ секунд"), [approved])

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
        self.assertIn(b'fstack_status: "approved"', stale)
        self.assertIn(
            f'supersedes_link: "[[Codex Memory/Projects/project/Records/{previous["id"]}]]"'.encode(),
            stale,
        )
        files = memory_store._files
        forget_results = []

        def forget_before_scanning_records(directory):
            if directory.name == "Records":
                forget_results.append(self.store.forget(binding, correction["id"]))
            return files(directory)

        with patch("memory_store._files", side_effect=forget_before_scanning_records):
            self.assertEqual(self.store.search(binding, "порог"), [])
        self.assertEqual(forget_results, [{"id": correction["id"], "status": "forgotten"}])
        tombstone = self.entry_path(binding, "Forgotten", correction["id"])
        self.assertNotIn(correction["body"], tombstone.read_text(encoding="utf-8"))
        self.assertIn(f'supersedes: "{previous["id"]}"', tombstone.read_text(encoding="utf-8"))
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

    def test_forget_can_remove_a_damaged_record_by_id(self):
        binding = self.setup_store()
        draft = self.store.save(binding, self.candidate, approved=False)
        path = self.entry_path(binding, "Records", draft["id"])
        path.write_bytes(b"\xff")
        self.assertEqual(self.store.forget(binding, draft["id"]), {"id": draft["id"], "status": "forgotten"})
        self.assertFalse(path.exists())
        path.write_bytes(b"\xff")
        self.assertEqual(self.store.inbox(binding), [])
        self.assertEqual(self.store.search(binding, "порог"), [])

    def test_interrupted_forget_publishes_known_supersedes_before_capture(self):
        binding = self.setup_store()
        previous = self.store.save(binding, self.candidate, approved=True)
        changed = dict(self.candidate, body="Использовать пять секунд.", supersedes=previous["id"])
        correction = self.store.save(binding, changed, approved=True)
        capture = memory_store._capture_record

        def interrupt_after_capture(record, captured):
            self.assertTrue(capture(record, captured))
            raise StoreError("Interrupted after capture")

        with patch("memory_store._capture_record", side_effect=interrupt_after_capture):
            with self.assertRaisesRegex(StoreError, "Interrupted after capture"):
                self.store.forget(binding, correction["id"])
        tombstone = self.entry_path(binding, "Forgotten", correction["id"])
        self.assertIn(f'supersedes: "{previous["id"]}"', tombstone.read_text(encoding="utf-8"))
        self.assertFalse(self.entry_path(binding, "Records", correction["id"]).exists())
        self.store.forget(binding, correction["id"])

    def test_late_approved_copy_preserves_supersedes_when_forgotten(self):
        for operation in ("forget", "migrate", "save"):
            with self.subTest(operation=operation):
                binding = self.store.setup(self.vault, self.repo, operation, "auto")
                previous = self.store.save(binding, self.candidate, approved=True)
                other = self.store.save(binding, dict(self.candidate, title="Unrelated"), approved=True)
                candidate = dict(self.candidate, body="New threshold.", supersedes=previous["id"])
                draft = self.store.save(binding, candidate, approved=False)
                path = self.entry_path(binding, "Records", draft["id"])
                draft_text = path.read_text(encoding="utf-8")
                forgotten = self.entry_path(binding, "Forgotten", draft["id"])
                self.store.forget(binding, draft["id"])
                self.assertEqual(self.store.get(binding, previous["id"]), previous)
                self.assertNotIn("supersedes:", forgotten.read_text(encoding="utf-8"))
                approved = draft_text.replace("fstack_memory: 1", 'fstack_memory: 1\nfstack_status: "approved"')
                if operation == "save":
                    original = forgotten.read_bytes()
                    forgotten.unlink()
                    write_text = memory_store._write_text

                    def receive_tombstone(target, text, **options):
                        result = write_text(target, text, **options)
                        if target == path:
                            forgotten.write_bytes(original)
                        return result

                    with patch("memory_store._write_text", side_effect=receive_tombstone):
                        with self.assertRaisesRegex(StoreError, "unavailable"):
                            self.store.save(binding, candidate, approved=True)
                else:
                    path.write_text(approved, encoding="utf-8")
                    self.assertEqual(self.store.search(binding, "конфигурации"), [other])
                    with self.assertRaises(StoreError):
                        self.store.get(binding, draft["id"])
                    if operation == "migrate":
                        self.assertEqual(self.store.migrate(binding), {"updated": 1, "skipped": 0})
                        self.assertTrue(path.exists())
                        path.unlink()
                    else:
                        self.store.forget(binding, draft["id"])
                self.assertFalse(path.exists())
                self.assertIn(previous["id"], forgotten.read_text(encoding="utf-8"))
                self.assertEqual(self.store.search(binding, "конфигурации"), [other])
                path.write_text(approved.replace(previous["id"], other["id"]), encoding="utf-8")
                self.assertEqual(self.store.search(binding, "конфигурации"), [other])
                self.assertEqual(self.store.migrate(binding), {"updated": 0, "skipped": 1})
                self.store.forget(binding, draft["id"])
                self.assertEqual(self.store.search(binding, "конфигурации"), [other])

    def test_unrelated_draft_metadata_does_not_block_other_entries(self):
        binding = self.setup_store()
        draft = self.store.save(binding, self.candidate, approved=False)
        path = self.entry_path(binding, "Records", draft["id"])
        raw = path.read_text(encoding="utf-8")
        path.write_text(raw.replace("fstack_memory: 1", "fstack_memory: 1\ntags: [work]"), encoding="utf-8")
        other = self.store.save(binding, dict(self.candidate, title="Searchable"), approved=True)
        self.assertEqual(self.store.search(binding, "Searchable"), [other])
        self.assertEqual(self.store.get(binding, other["id"]), other)
        with self.assertRaisesRegex(StoreError, "metadata"):
            self.store.approve(binding, draft["id"])

    def test_forget_captures_approval_delivered_after_creating_tombstone(self):
        binding = self.setup_store()
        previous = self.store.save(binding, self.candidate, approved=True)
        other = self.store.save(binding, dict(self.candidate, title="Unrelated"), approved=True)
        candidate = dict(self.candidate, body="New threshold.", supersedes=previous["id"])
        draft = self.store.save(binding, candidate, approved=False)
        path = self.entry_path(binding, "Records", draft["id"])
        tombstone = self.entry_path(binding, "Forgotten", draft["id"])
        approved = path.read_text(encoding="utf-8").replace(
            "fstack_memory: 1", 'fstack_memory: 1\nfstack_status: "approved"',
        )
        write_text = memory_store._write_text

        def deliver_approval_then_interrupt_update(target, text, **options):
            if target == tombstone and options.get("replace"):
                raise StoreError("interrupted before saving predecessor")
            written = write_text(target, text, **options)
            if target == tombstone:
                path.write_text(approved, encoding="utf-8")
            return written

        with patch("memory_store._write_text", side_effect=deliver_approval_then_interrupt_update):
            with self.assertRaisesRegex(StoreError, "interrupted"):
                self.store.forget(binding, draft["id"])
        self.assertFalse(path.exists())
        self.assertNotIn("supersedes:", tombstone.read_text(encoding="utf-8"))
        hidden, = path.parent.glob(".fstack-forget-*")
        self.assertEqual(hidden.read_text(encoding="utf-8"), approved)
        self.assertEqual(self.store.search(binding, "конфигурации"), [other])
        path.write_text(approved.replace(previous["id"], other["id"]), encoding="utf-8")
        record_approval = memory_store._record_approval

        def finish_forget_before_reader_sees_capture(target, entry_id):
            with patch("memory_store._record_approval", side_effect=record_approval):
                self.store.forget(binding, draft["id"])
            return record_approval(target, entry_id)

        with patch("memory_store._record_approval", side_effect=finish_forget_before_reader_sees_capture):
            self.assertEqual(self.store.search(binding, "конфигурации"), [other])
        self.assertFalse(path.exists())
        self.assertFalse(hidden.exists())
        self.assertIn(previous["id"], tombstone.read_text(encoding="utf-8"))
        marker = self.legacy_approval(binding, draft["id"], other["id"])
        self.assertEqual(self.store.search(binding, "конфигурации"), [other])
        self.assertEqual(self.store.migrate(binding), {"updated": 0, "skipped": 1})
        self.assertEqual(self.store.migrate(binding, prune_legacy=True), {"pruned": 0, "skipped": 1})
        self.assertTrue(marker.exists())

    def test_added_tags_do_not_reactivate_an_approved_predecessor(self):
        binding = self.setup_store()
        previous = self.store.save(binding, self.candidate, approved=True)
        candidate = dict(self.candidate, body="New threshold.", supersedes=previous["id"])
        approved = self.store.save(binding, candidate, approved=True)
        path = self.entry_path(binding, "Records", approved["id"])
        raw = path.read_text(encoding="utf-8")
        path.write_text(raw.replace("fstack_memory: 1", "fstack_memory: 1\ntags: [work]"), encoding="utf-8")
        with self.assertRaisesRegex(StoreError, "unavailable"):
            self.store.get(binding, previous["id"])
        with self.assertRaisesRegex(StoreError, "metadata"):
            self.store.get(binding, approved["id"])
        self.store.forget(binding, approved["id"])
        self.assertEqual(self.store.search(binding, "порог"), [])

    def test_large_library_does_not_block_memory_operations(self):
        binding = self.setup_store()
        library = binding.root / "Library.base"
        large = b"#" + b" " * memory_store.MAX_FILE + b"\n" + library.read_bytes()
        library.write_bytes(large)
        self.assertEqual(self.setup_store(), binding)
        draft = self.store.save(binding, self.candidate, approved=False)
        self.assertEqual(self.store.approve(binding, draft["id"])["status"], "memory")
        with patch("memory_store.ensure_library", side_effect=OSError("cannot update UI")):
            saved = self.store.save(binding, dict(self.candidate, title="Saved"), approved=True)
        self.assertEqual(self.store.get(binding, saved["id"]), saved)
        self.assertEqual(self.store.migrate(binding), {"updated": 0, "skipped": 0})
        self.assertEqual(library.read_bytes(), large)

    def test_graph_links_allow_valid_slugs_that_resemble_secrets(self):
        binding = self.store.setup(self.vault, self.repo, "sk-platform-integration", "auto")
        previous = self.store.save(binding, self.candidate, approved=True)
        candidate = dict(self.candidate, body="New threshold.", supersedes=previous["id"])
        draft = self.store.save(binding, candidate, approved=False)
        approved = self.store.approve(binding, draft["id"])
        self.assertEqual(self.store.get(binding, draft["id"]), approved)
        path = self.entry_path(binding, "Records", draft["id"])
        raw = path.read_text(encoding="utf-8")
        self.assertIn("[[Codex Memory/Projects/sk-platform-integration/Project|sk-platform-integration]]", raw)
        self.assertIn(f'[[Codex Memory/Projects/sk-platform-integration/Records/{previous["id"]}]]', raw)

    def test_migration_reads_legacy_in_either_sync_order_and_prunes_only_equivalent_markers(self):
        binding = self.setup_store()
        previous = self.store.save(binding, self.candidate, approved=True)
        other = self.store.save(binding, dict(self.candidate, title="Other entry"), approved=True)
        candidate = dict(self.candidate, body="Use a new threshold.", supersedes=other["id"])
        draft = self.store.save(binding, candidate, approved=False)
        path = self.entry_path(binding, "Records", draft["id"])
        legacy = "\n".join(
            line for line in path.read_text(encoding="utf-8").split("\n")
            if not line.startswith(("project_link:", "supersedes_link:"))
        )
        path.write_text(legacy, encoding="utf-8")
        self.assertEqual(self.store.inbox(binding), [draft])
        path.unlink()
        marker = self.legacy_approval(binding, draft["id"], previous["id"])
        orphan = self.legacy_approval(binding, "a" * 64)
        with self.assertRaises(StoreError):
            self.store.get(binding, previous["id"])
        path.write_text(legacy.replace("Use a new threshold.", "A manually revised threshold."), encoding="utf-8")
        approved = self.store.get(binding, draft["id"])
        self.assertEqual(approved["supersedes"], previous["id"])
        self.assertEqual(self.store.get(binding, other["id"]), other)
        unknown = self.legacy_approval(binding, "b" * 64)
        unknown.write_text("Unrecognized content", encoding="utf-8")
        self.assertEqual(self.store.migrate(binding, prune_legacy=True), {"pruned": 0, "skipped": 3})
        self.assertTrue(marker.exists())
        with patch("memory_store._write_text", side_effect=StoreError("write failed")):
            with self.assertRaisesRegex(StoreError, "write failed"):
                self.store.migrate(binding)
        self.assertTrue(marker.exists())
        self.assertNotIn("fstack_status", path.read_text(encoding="utf-8"))
        self.assertEqual(self.store.migrate(binding), {"updated": 1, "skipped": 2})
        updated = path.read_text(encoding="utf-8")
        self.assertIn('project_link: "[[Codex Memory/Projects/project/Project|project]]"', updated)
        self.assertIn(f'supersedes_link: "[[Codex Memory/Projects/project/Records/{previous["id"]}]]"', updated)
        self.assertEqual(self.store.migrate(binding), {"updated": 0, "skipped": 2})
        self.assertTrue(marker.exists())
        path.write_text(updated.replace(f'supersedes: "{previous["id"]}"', f'supersedes: "{other["id"]}"'), encoding="utf-8")
        self.assertEqual(self.store.migrate(binding, prune_legacy=True), {"pruned": 0, "skipped": 3})
        path.write_text(updated, encoding="utf-8")
        self.assertEqual(self.store.migrate(binding, prune_legacy=True), {"pruned": 1, "skipped": 2})
        self.assertFalse(marker.exists())
        self.assertTrue(orphan.exists())
        self.assertTrue(unknown.exists())
        unknown.unlink()
        self.assertEqual(self.store.get(binding, draft["id"]), approved)
        self.assertEqual(approved["body"], "A manually revised threshold.")

    def test_migration_preserves_legacy_supersedes_in_forgotten_note(self):
        binding = self.setup_store()
        previous = self.store.save(binding, self.candidate, approved=True)
        draft = self.store.save(binding, dict(self.candidate, body="Revised threshold."), approved=False)
        path = self.entry_path(binding, "Records", draft["id"])
        tombstone = self.entry_path(binding, "Forgotten", draft["id"])
        tombstone.write_text(f'---\nfstack_forgotten: 1\nid: "{draft["id"]}"\n---\n', encoding="utf-8")
        marker = self.legacy_approval(binding, draft["id"], previous["id"])
        self.assertEqual(self.store.migrate(binding, prune_legacy=True), {"pruned": 0, "skipped": 1})
        self.assertEqual(self.store.inbox(binding), [])
        self.assertEqual(self.store.migrate(binding), {"updated": 1, "skipped": 0})
        self.assertIn(previous["id"], tombstone.read_text(encoding="utf-8"))
        self.assertTrue(marker.exists())
        self.assertEqual(self.store.migrate(binding, prune_legacy=True), {"pruned": 1, "skipped": 0})
        self.assertEqual(self.store.migrate(binding), {"updated": 0, "skipped": 0})
        self.assertTrue(path.exists())
        self.assertEqual(self.store.inbox(binding), [])
        self.assertEqual(self.store.search(binding, "порог"), [])

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

        def fail_approval(path, text, **options):
            if path.parent.name == "Records" and 'fstack_status: "approved"' in text:
                raise StoreError("interrupted before publishing approval")
            return write_text(path, text, **options)

        with patch("memory_store._write_text", side_effect=fail_approval), self.assertRaises(StoreError):
            self.store.save(binding, self.candidate, approved=True)
        self.assertEqual(self.store.inbox(binding), [])
        draft = self.store.save(binding, self.candidate, approved=False)
        record = self.entry_path(binding, "Records", draft["id"])
        original = record.read_bytes()
        with patch("memory_store._write_text", side_effect=fail_approval), self.assertRaises(StoreError):
            self.store.approve(binding, draft["id"])
        self.assertEqual(record.read_bytes(), original)
        self.assertEqual(self.store.search(binding, "порог"), [])
        revised = record.read_text(encoding="utf-8").replace("три секунды", "пять секунд")
        def edit_before_approval(path, text, **options):
            if path == record:
                record.write_text(revised, encoding="utf-8")
            return write_text(path, text, **options)

        with patch("memory_store._write_text", side_effect=edit_before_approval):
            with self.assertRaisesRegex(StoreError, "changed"):
                self.store.approve(binding, draft["id"])
        self.assertEqual(record.read_text(encoding="utf-8"), revised)
        approved = self.store.save(binding, self.candidate, approved=True)
        self.assertEqual(approved["id"], draft["id"])
        self.assertIn("пять секунд", approved["body"])
        self.assertIn("пять секунд", record.read_text(encoding="utf-8"))
        self.assertEqual(self.store.get(binding, approved["id"]), approved)

    def test_promotion_cannot_publish_a_record_above_read_limit(self):
        binding = self.setup_store()
        draft = self.store.save(binding, self.candidate, approved=False)
        record = self.entry_path(binding, "Records", draft["id"])
        original = record.read_bytes()
        padded = original + b" " * (memory_store.MAX_FILE - len(original))
        record.write_bytes(padded)
        with self.assertRaisesRegex(StoreError, "too large"):
            self.store.approve(binding, draft["id"])
        self.assertEqual(record.read_bytes(), padded)
        self.assertEqual(self.store.inbox(binding), [draft])

    def test_approval_and_forget_follow_lock_order_across_profiles(self):
        for first in ("approve", "forget"):
            with self.subTest(first=first):
                self.assert_approval_and_forget_order(first)

    def assert_approval_and_forget_order(self, first):
        binding = self.store.setup(self.vault, self.repo, first, "auto")
        other_store = Store(self.temp / "other-profile")
        other_binding = other_store.setup(self.vault, self.repo, first, "auto")
        predecessor = self.store.save(binding, self.candidate, approved=True)
        correction = dict(self.candidate, body="Использовать пять секунд.", supersedes=predecessor["id"])
        draft = self.store.save(binding, correction, approved=False)
        record = self.entry_path(binding, "Records", draft["id"])
        tombstone = self.entry_path(binding, "Forgotten", draft["id"])
        original = record.read_bytes()
        paused, contended, resume = Event(), Event(), Event()
        write_text = memory_store._write_text
        try_lock = memory_store._try_project_lock
        first_path = record if first == "approve" else tombstone
        second = "forget" if first == "approve" else "approve"

        def pause_first_marker(path, text, **options):
            if path == first_path:
                paused.set()
                self.assertTrue(resume.wait(5), "first operation was not released")
            return write_text(path, text, **options)

        def observe_contention(descriptor):
            try:
                try_lock(descriptor)
            except OSError:
                contended.set()
                raise

        with (patch("memory_store._write_text", side_effect=pause_first_marker),
              patch("memory_store._try_project_lock", side_effect=observe_contention),
              ThreadPoolExecutor(max_workers=2) as pool):
            first_result = pool.submit(getattr(self.store, first), binding, draft["id"])
            try:
                self.assertTrue(paused.wait(5), "first operation did not reach marker")
                second_result = pool.submit(getattr(other_store, second), other_binding, draft["id"])
                self.assertTrue(contended.wait(2), "second operation did not contend for the same lock")
            finally:
                resume.set()
            if first == "approve":
                self.assertEqual(first_result.result()["status"], "memory")
                self.assertEqual(second_result.result()["status"], "forgotten")
                self.assertIn(predecessor["id"], tombstone.read_text(encoding="utf-8"))
                expected = []
            else:
                self.assertEqual(first_result.result()["status"], "forgotten")
                with self.assertRaises(StoreError):
                    second_result.result()
                self.assertNotIn(predecessor["id"], tombstone.read_text(encoding="utf-8"))
                expected = [predecessor]
        self.assertFalse(record.exists())
        self.assertEqual(self.store.search(binding, "порог"), expected)
        record.write_bytes(original)
        self.assertEqual(self.store.search(binding, "порог"), expected)
        self.assertEqual(self.store.inbox(binding), [])

    def test_mutation_fails_closed_when_lock_is_busy_or_unavailable(self):
        binding = self.setup_store()
        project = binding.root / "Projects" / binding.project
        with memory_store._mutation_lock(project, self.vault):
            with patch("memory_store.LOCK_TIMEOUT", 0), self.assertRaisesRegex(StoreError, "Timed out"):
                self.store.save(binding, self.candidate, approved=True)
        with patch("memory_store._try_project_lock", side_effect=OSError(errno.ENOSYS, "unavailable")):
            with self.assertRaisesRegex(StoreError, "Cannot acquire"):
                self.store.save(binding, self.candidate, approved=True)
        inside_vault = self.vault / "unsafe-locks"
        with patch("memory_store._lock_directory", return_value=inside_vault):
            with self.assertRaisesRegex(StoreError, "inside the vault"):
                self.store.save(binding, self.candidate, approved=True)
        self.assertFalse(inside_vault.exists())
        self.assertEqual(list((project / "Records").iterdir()), [])
        entry = self.store.save(binding, self.candidate, approved=True)
        self.assertEqual(entry["status"], "memory")

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
