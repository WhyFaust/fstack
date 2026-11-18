"""Проверяет обнаружение и явную загрузку всех установленных навыков Codex."""
import argparse
import json
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from app_server import Server, environment


class ResponsesHandler(BaseHTTPRequestHandler):
    def do_POST(self) -> None:
        if self.path != "/v1/responses":
            self.send_error(404)
            return
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.inputs.append(request["input"])
        message = {"id": "msg_1", "type": "message", "role": "assistant", "status": "completed",
                   "content": [{"type": "output_text", "text": "OK", "annotations": []}]}
        response = {"id": "resp_1", "object": "response", "status": "completed", "output": [message]}
        events = [
            {"type": "response.created", "response": {**response, "status": "in_progress", "output": []}},
            {"type": "response.output_item.done", "output_index": 0, "item": message},
            {"type": "response.completed", "response": response},
        ]
        body = "".join(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n" for event in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args) -> None:
        pass


def check(package: Path, codex: str, scratch: Path) -> None:
    home = scratch / "profile"
    workspace = scratch / "workspace"
    home.mkdir()
    workspace.mkdir()

    def run(*args: str) -> str:
        result = subprocess.run([codex, *args], cwd=workspace, env=environment(home),
                                stdin=subprocess.DEVNULL, capture_output=True, encoding="utf-8", timeout=120)
        if result.returncode:
            raise RuntimeError(f"codex {args[0]}: {result.stderr.strip()}")
        return result.stdout

    with ThreadingHTTPServer(("127.0.0.1", 0), ResponsesHandler) as provider:
        provider.inputs = []
        (home / "config.toml").write_text(f'''model = "gpt-6.2-sol"
model_provider = "fixture"
check_for_update_on_startup = false
[features]
apps = false
hooks = false
remote_plugin = false
[model_providers.fixture]
name = "Local fixture"
base_url = "http://127.0.0.1:{provider.server_port}/v1"
wire_api = "responses"
supports_websockets = false
request_max_retries = 0
stream_max_retries = 0
''', encoding="utf-8")
        run("plugin", "marketplace", "add", str(package))
        installed = json.loads(run("plugin", "add", "fstack@fstack", "--json"))
        root = Path(installed["installedPath"]).resolve()
        sources = {f"fstack:{entry.parent.name}": entry.read_text(encoding="utf-8").replace("\r\n", "\n").strip()
                   for entry in sorted((package / "skills").glob("*/SKILL.md"))}
        entries = {f"fstack:{entry.parent.name}": entry.resolve()
                   for entry in (root / "skills").glob("*/SKILL.md")}
        if not sources or sources.keys() != entries.keys():
            raise RuntimeError("Набор установленных навыков отличается от пакета")
        for name, source in sources.items():
            if entries[name].read_text(encoding="utf-8").replace("\r\n", "\n").strip() != source:
                raise RuntimeError(f"Установленная инструкция {name} отличается от пакета")

        server = Server(codex, home, workspace)
        thread = threading.Thread(target=provider.serve_forever)
        thread.start()
        try:
            server.initialize(time.monotonic() + 60)
            groups = server.request("skills/list", {"cwds": [str(workspace)], "forceReload": True},
                                    time.monotonic() + 60)["data"]
            if not groups or any(group.get("errors") for group in groups):
                raise RuntimeError(f"Ошибки обнаружения навыков: {groups}")
            discovered = [skill for group in groups for skill in group["skills"]
                          if skill.get("pluginId") == "fstack@fstack"]
            if (len(discovered) != len(entries) or {skill["name"] for skill in discovered} != entries.keys()
                    or any(skill.get("enabled") is not True
                           or Path(skill["path"]).resolve() != entries[skill["name"]] for skill in discovered)):
                raise RuntimeError("Codex не обнаружил все включённые навыки по установленным путям")
            cases = [("Проверь изменение.", None)]
            cases.extend((f"${name} Проверь доступность инструкции.", name) for name in sources)
            for prompt, selected in cases:
                provider.inputs.clear()
                deadline = time.monotonic() + 60
                started = server.request("thread/start", {
                    "cwd": str(workspace), "ephemeral": True, "modelProvider": "fixture",
                    "approvalPolicy": "never", "sandbox": "read-only",
                }, deadline)
                thread_id = started["thread"]["id"]
                server.request("turn/start", {"threadId": thread_id,
                               "input": [{"type": "text", "text": prompt}]}, deadline)
                while True:
                    msg = server.pending.pop(0) if server.pending else server.receive(deadline)
                    if msg.get("method") == "turn/completed" and msg["params"]["threadId"] == thread_id:
                        if msg["params"]["turn"]["status"] != "completed":
                            raise RuntimeError(f"Локальный запрос не завершён: {msg['params']['turn'].get('error')}")
                        break
                if len(provider.inputs) != 1:
                    raise RuntimeError(f"Ожидался один локальный запрос, получено {len(provider.inputs)}")
                texts = [part["text"].replace("\r\n", "\n") for item in provider.inputs[0]
                         for part in item.get("content", []) if part.get("type") == "input_text"]
                loaded = {name for name, source in sources.items() if any(source in text for text in texts)}
                expected = {selected} if selected is not None else set()
                if prompt not in texts or loaded != expected:
                    raise RuntimeError(
                        f"Неверная загрузка SKILL.md для {prompt!r}. "
                        f"Ожидалось {sorted(expected)}, получено {sorted(loaded)}."
                    )
        finally:
            server.close()
            provider.shutdown()
            thread.join()
    print(f"{run('--version').strip()}: обнаружено {len(sources)} навыков; "
          f"{len(sources) + 1} локальных запросов подтвердили явную загрузку каждого "
          "и отсутствие полной инструкции при обычном запросе.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=Path)
    parser.add_argument("--codex", default="codex")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="fstack-skill-check-") as directory:
        check(args.package.resolve(), args.codex, Path(directory))
