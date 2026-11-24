#!/usr/bin/env python3
"""Proves that the real Asqu program links to a local AsquHub and that every save arrives.

What it does, all in a new throwaway folder (your own ~/.asqu and ~/.asquhub are never used):
  1. starts a Hub with `asquhub start --no-browser` on a free port between 5201 and 5209
     (or the range in ASQU_PROOF_PORTS, such as 5221-5229);
  2. makes a small project and saves it with Asqu;
  3. runs `asqu link hub` and approves the link the way the owner's browser does, with a
     software passkey made just for this run (the Hub's own tests do the same);
  4. makes more saves (a change, a new runnable file, a deletion) and waits for Asqu's
     background copier to send each one, with no extra command;
  5. checks the Hub's history: one version per save, in order, with Asqu's messages and
     signatures, and the latest version's files byte for byte the same as the folder;
  6. stops the Hub and waits for Asqu's background copier to finish.

Needs: python3 (standard library only), openssl and curl. Asqu must be a development build
(`cargo build`): a released Asqu asks the person at a terminal before linking a Hub on this
computer. Point at the programs with ASQU_BIN and ASQUHUB_BIN, or build both side by side:
  ../asqu/target/debug/asqu and ../asquhub/target/debug/asquhub.

Run: python3 scripts/hub-link-proof.py   (exit code 0 means everything was proven)
"""

import base64
import hashlib
import http.client
import json
import os
import re
import signal
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
ASQU = os.environ.get("ASQU_BIN", str(HERE / "target/debug/asqu"))
ASQUHUB = os.environ.get("ASQUHUB_BIN", str(HERE.parent / "asquhub/target/debug/asquhub"))

def port_range(default: str) -> range:
    """ASQU_PROOF_PORTS (such as "5221-5229"), or `default`."""
    first, last = os.environ.get("ASQU_PROOF_PORTS", default).split("-")
    return range(int(first), int(last) + 1)


PORTS = port_range("5201-5209")
UP_UV = 0x05  # the person was present and verified (Face ID / Touch ID)


def step(text):
    print(f"\n== {text}", flush=True)


def fail(text):
    print(f"\nFAILED: {text}", flush=True)
    raise SystemExit(1)


def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def free_port() -> int:
    for port in PORTS:
        with socket.socket() as s:
            try:
                s.bind(("127.0.0.1", port))
                return port
            except OSError:
                continue
    fail(f"no free port between {PORTS.start} and {PORTS.stop - 1}")


class Hub:
    """Talks to the Hub as the owner's key (bearer) or the owner's browser (cookie)."""

    def __init__(self, port: int, owner_token: str):
        self.port = port
        self.owner = owner_token
        self.cookie = None
        self.csrf = None

    def request(self, method, path, body=None, token=None, browser=False):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=30)
        headers = {}
        data = None
        if body is not None:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        if browser:
            # Passkeys work at localhost, so the browser uses that name.
            headers["Host"] = f"localhost:{self.port}"
            headers["Origin"] = f"http://localhost:{self.port}"
            if self.cookie:
                headers["Cookie"] = self.cookie
            if self.csrf and method != "GET":
                headers["X-AsquHub-CSRF"] = self.csrf
        elif token:
            headers["Authorization"] = f"Bearer {token}"
        conn.request(method, path, body=data, headers=headers)
        r = conn.getresponse()
        raw = r.read()
        conn.close()
        try:
            value = json.loads(raw) if raw else None
        except ValueError:
            value = None
        return r.status, value, raw, r.getheader("Set-Cookie")

    def owner_get(self, path):
        status, value, raw, _ = self.request("GET", path, token=self.owner)
        if status != 200:
            fail(f"GET {path} answered {status}: {raw[:300]!r}")
        return value, raw


class SoftPasskey:
    """A passkey made with openssl for this run, answering like a phone or Mac would."""

    def __init__(self, folder: Path):
        self.key = folder / "passkey.pem"
        subprocess.run(["openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", str(self.key)],
                       check=True, capture_output=True)
        self.spki = subprocess.run(["openssl", "pkey", "-in", str(self.key), "-pubout", "-outform", "DER"],
                                   check=True, capture_output=True).stdout
        self.id = b"proof-script-passkey"
        self.counter = 0
        self.rp = hashlib.sha256(b"localhost").digest()

    def sign(self, data: bytes) -> bytes:
        return subprocess.run(["openssl", "dgst", "-sha256", "-sign", str(self.key)],
                              input=data, check=True, capture_output=True).stdout

    def register(self, hub: Hub):
        status, options, raw, _ = hub.request("POST", "/v1/passkeys/register-options", browser=True)
        if status != 200:
            fail(f"register-options answered {status}: {raw[:300]!r}")
        challenge = options["challenge"]
        data = self.rp + bytes([0x45]) + (0).to_bytes(4, "big") + bytes(16)
        data += len(self.id).to_bytes(2, "big") + self.id
        client = json.dumps({"type": "webauthn.create", "challenge": challenge, "origin": f"http://localhost:{hub.port}"})
        body = {
            "challenge": challenge,
            "passkey_id": b64url(self.id),
            "client_data_json": b64url(client.encode()),
            "authenticator_data": b64url(data),
            "public_key": b64url(self.spki),
            "algorithm": -7,
            "name": "Proof script passkey",
        }
        status, _, raw, _ = hub.request("POST", "/v1/passkeys", body, browser=True)
        if status != 201:
            fail(f"adding the passkey answered {status}: {raw[:300]!r}")

    def check(self, hub: Hub, items):
        status, options, raw, _ = hub.request("POST", "/v1/checks", {"items": items}, browser=True)
        if status != 200:
            fail(f"/v1/checks answered {status}: {raw[:300]!r}")
        self.counter += 1
        challenge = options["challenge"]
        data = self.rp + bytes([UP_UV]) + self.counter.to_bytes(4, "big")
        client = json.dumps({"type": "webauthn.get", "challenge": challenge, "origin": f"http://localhost:{hub.port}"})
        signature = self.sign(data + hashlib.sha256(client.encode()).digest())
        return {
            "challenge": challenge,
            "passkey_id": b64url(self.id),
            "client_data_json": b64url(client.encode()),
            "authenticator_data": b64url(data),
            "signature": b64url(signature),
        }


def browser_sign_in(hub: Hub):
    status, link, raw, _ = hub.request("POST", "/v1/sign-in-links", {}, token=hub.owner)
    if status != 201:
        fail(f"sign-in link answered {status}: {raw[:300]!r}")
    path = "/sign-in" + link["url"].split("/sign-in", 1)[1]
    _, _, _, cookie = hub.request("GET", path, browser=True)
    if not cookie:
        fail("the sign-in link set no cookie")
    hub.cookie = cookie.split(";", 1)[0]
    status, session, raw, _ = hub.request("GET", "/v1/session", browser=True)
    if status != 200:
        fail(f"/v1/session answered {status}: {raw[:300]!r}")
    hub.csrf = session["csrf"]


def run_asqu(project: Path, env, *args, check=True):
    r = subprocess.run([ASQU, *args], cwd=project, env=env, capture_output=True, text=True, timeout=120)
    out = (r.stdout + r.stderr).strip()
    print(f"$ asqu {' '.join(args)}\n{out}", flush=True)
    if check and r.returncode != 0:
        fail(f"asqu {' '.join(args)} exited {r.returncode}")
    return out


def wait_for(what, test, seconds=60):
    deadline = time.time() + seconds
    while time.time() < deadline:
        value = test()
        if value:
            return value
        time.sleep(0.5)
    fail(f"timed out waiting for {what}")


def main():
    for program in (ASQU, ASQUHUB):
        if not os.access(program, os.X_OK):
            fail(f"{program} not found; build it first (cargo build) or set ASQU_BIN / ASQUHUB_BIN")
    base = Path(tempfile.mkdtemp(prefix="asqu-hub-proof-", dir=os.environ.get("TMPDIR")))
    hub_home, asqu_home, project = base / "hub-home", base / "asqu-home", base / "proof-project"
    asqu_home.mkdir()
    project.mkdir()
    print(f"Throwaway folder: {base}")

    env = dict(os.environ)
    env.update({"ASQU_HOME": str(asqu_home), "ASQU_NO_BROWSER": "1", "ASQU_AGENT": "Proof script"})
    env.pop("ASQU_AUTO_BACKUP", None)  # the real background copier sends saves
    hub_env = dict(os.environ)
    hub_env.pop("ASQUHUB_HOME", None)

    port = free_port()
    step(f"Start a Hub on port {port}")
    hub_log = open(base / "hub.log", "w")
    hub_proc = subprocess.Popen([ASQUHUB, "start", "--home", str(hub_home), "--port", str(port), "--no-browser",
                                 "--name", "Proof owner"], stdout=hub_log, stderr=subprocess.STDOUT, env=hub_env)
    link_proc = None
    try:
        def hub_up():
            if hub_proc.poll() is not None:
                fail(f"the Hub stopped: {(base / 'hub.log').read_text()}")
            try:
                c = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
                c.request("GET", "/healthz")
                return c.getresponse().status == 200
            except OSError:
                return False
        wait_for("the Hub to start", hub_up, 30)
        owner = json.loads((hub_home / "owner.json").read_text())["token"]
        hub = Hub(port, owner)
        print("Hub is running; owner key read from the throwaway hub-home.")

        step("Make a project and save it with Asqu")
        (project / "README.md").write_text("# Proof project\n")
        (project / "notes.txt").write_text("first line\n")
        run_asqu(project, env, "save", "--why", "Start the proof project")

        step("asqu link hub, approved with a software passkey")
        link_proc = subprocess.Popen([ASQU, "link", "hub", "--hub", f"http://127.0.0.1:{port}"], cwd=project, env=env,
                                     stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        code = None
        lines = []
        for line in link_proc.stdout:
            lines.append(line.rstrip())
            print(f"  asqu: {line.rstrip()}", flush=True)
            m = re.search(r"check the code matches (\S+)", line)
            if m:
                code = m.group(1)
                break
        if not code:
            fail("asqu link hub showed no code")
        browser_sign_in(hub)
        passkey = SoftPasskey(base)
        passkey.register(hub)
        status, waiting, raw, _ = hub.request("GET", f"/v1/link/requests/{code}", browser=True)
        if status != 200:
            fail(f"the owner could not see the link request ({status}): {raw[:300]!r}")
        print(f"  Approval page shows: {json.dumps(waiting)}")
        choice = {"new_project_name": "Proof project", "level": "change"}
        check = passkey.check(hub, [dict(choice, kind="link", code=code)])
        status, approved, raw, _ = hub.request("POST", f"/v1/link/requests/{code}/approve", dict(choice, check=check),
                                               browser=True)
        if status != 200:
            fail(f"approving the link answered {status}: {raw[:300]!r}")
        rest, _ = link_proc.communicate(timeout=60)
        for line in rest.splitlines():
            print(f"  asqu: {line}")
            lines.append(line)
        if link_proc.returncode != 0 or not any("Shared the history so far" in l for l in lines):
            fail("asqu link hub did not finish by sharing the history")

        links = json.loads((asqu_home / "hub-links.json").read_text())
        link = next(iter(links.values()))
        project_id, key = link["project_id"], link["token"].encode()
        for p in project.rglob("*"):
            if p.is_file() and key in p.read_bytes():
                fail(f"the Hub key ended up inside the project, in {p}")
        print(f"  Linked to Hub project {project_id}; key kept only in the throwaway ASQU_HOME.")

        step("More saves; the background copier sends each one by itself")
        (project / "notes.txt").write_text("first line\nsecond line\n")
        run_asqu(project, env, "save", "--why", "Add a second line")
        (project / "run.sh").write_text("#!/bin/sh\necho hello\n")
        os.chmod(project / "run.sh", 0o755)
        run_asqu(project, env, "save", "--why", "Add a runnable script")
        (project / "README.md").unlink()
        run_asqu(project, env, "save", "--why", "Remove the readme")

        def history():
            value, _ = hub.owner_get(f"/v1/projects/{project_id}/history")
            versions = value["revisions"]
            return versions if len(versions) >= 4 else None
        versions = wait_for("all four saves on the Hub", history, 60)

        step("What the Hub's history says")
        for v in versions:
            print(f"  #{v.get('number')} {v.get('message')!r} by {v.get('author')!r} source={v.get('source')}"
                  f" signed by Asqu key {v.get('signed_by_key')}")
        want = ["Start the proof project", "Add a second line", "Add a runnable script", "Remove the readme"]
        got = [v.get("message") for v in versions]
        if got != want:
            fail(f"history messages {got} are not {want}")
        if not all(v.get("signed_by_key") for v in versions):
            fail("a version on the Hub carries no Asqu signature")

        accepted, _ = hub.owner_get(f"/v1/projects/{project_id}/accepted")
        files = {f["path"]: f for f in accepted["files"]}
        local = {p.relative_to(project).as_posix(): p for p in project.rglob("*")
                 if p.is_file() and ".asqu" not in p.relative_to(project).parts}
        if set(files) != set(local):
            fail(f"Hub files {sorted(files)} differ from the folder's {sorted(local)}")
        for path, f in files.items():
            status, _, raw, _ = hub.request("GET", f"/v1/projects/{project_id}/blobs/{f['blob']}", token=owner)
            if status != 200 or raw != local[path].read_bytes():
                fail(f"{path} on the Hub is not the same as in the folder")
            if bool(f.get("exec")) != bool(os.stat(local[path]).st_mode & 0o100):
                fail(f"{path}: runnable flag differs")
        print(f"  Latest Hub version has exactly the folder's files, byte for byte: {sorted(files)} (run.sh runnable)")

        status_out = run_asqu(project, env, "status")
        if "shared with the Hub" not in status_out:
            fail("asqu status doesn't say the project is shared")

        step("Proven")
        print("Link approved once; four saves arrived in order, signed, with no extra command; files match.")
        return 0
    finally:
        if link_proc and link_proc.poll() is None:
            link_proc.kill()
        # Let Asqu's background copier finish before the Hub goes away.
        lock = project / ".asqu" / "backup.lock"
        deadline = time.time() + 30
        while lock.exists() and time.time() < deadline:
            time.sleep(0.5)
        hub_proc.send_signal(signal.SIGINT)
        try:
            hub_proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            hub_proc.kill()
            hub_proc.wait()
        hub_log.close()
        print(f"Hub stopped. Logs and throwaway data left in {base}")


if __name__ == "__main__":
    sys.exit(main())
