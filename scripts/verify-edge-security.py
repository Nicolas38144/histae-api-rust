#!/usr/bin/env python3
"""Verify Compose exposure and Nginx log privacy using disposable, offline containers."""
import argparse
import json
import os
import subprocess
import tempfile
import time
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PROJECTS = ("histae-dashboard", "reverse-proxy", "histae-site")
REQUEST_ID = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sudo-docker", action="store_true")
    parser.add_argument("--nginx-image", default="nginx:1.28-alpine")
    args = parser.parse_args()
    command = ["sudo", "docker"] if args.sudo_docker else ["docker"]

    def docker(*arguments, check=True, env=None):
        result = subprocess.run(
            [*command, *arguments], cwd=ROOT, text=True, capture_output=True, timeout=60, env=env,
        )
        if check and result.returncode:
            raise RuntimeError(f"Docker {arguments[0]} failed: {result.stderr[-2000:]}")
        return result

    with tempfile.TemporaryDirectory(prefix="histae-edge-security-") as temporary:
        directory = Path(temporary)
        environment = directory / "fixture.env"
        environment.write_text(
            f"HISTAE_ENV_FILE={environment}\nHISTAE_STORAGE_HOST=storage.test.invalid\n"
            "POSTGRES_DB=histae-test\nPOSTGRES_ADMIN_PASSWORD=fixture-admin\n"
            "POSTGRES_MIGRATE_PASSWORD=fixture-migrate\nPOSTGRES_APP_PASSWORD=fixture-app\n"
            "REDIS_PASSWORD=fixture-redis\nPHOTO_MODERATION_TOKEN=fixture-moderation\n"
            "HISTAE_API_BIND_IP=0.0.0.0\nHISTAE_API_HOST_PORT=18080\nTRUST_PROXY=false\n"
        )
        configuration_env = dict(os.environ)
        configuration_env.update(line.split("=", 1) for line in environment.read_text().splitlines())

        def compose(*files):
            arguments = ["compose", "--env-file", str(environment)]
            for file in files:
                arguments.extend(["-f", str(file)])
            return json.loads(docker(*arguments, "config", "--format", "json", env=configuration_env).stdout)

        development = compose(ROOT / "compose.yaml", ROOT / "compose.dev.yaml")
        ports = development["services"]["api"]["ports"]
        assert any(p.get("host_ip") == "0.0.0.0" for p in ports), "LAN access changed"
        production = compose(ROOT / "compose.yaml", ROOT / "compose.production.yaml")
        assert not production["services"]["api"].get("ports"), "Production API exposes a port"
        assert production["services"]["api"]["environment"]["TRUST_PROXY"] == (
            "172.30.0.3/32,172.30.0.4/32"
        ), "Production proxy trust must stay narrowly scoped"
        edge = compose(ROOT.parent / "reverse-proxy/docker-compose.yml")
        for service in edge["services"].values():
            assert service["logging"] == {
                "driver": "json-file", "options": {"max-size": "10m", "max-file": "5"},
            }, "Proxy log rotation missing"
        print("Compose: LAN preserved, production API internal, trusted proxies and rotation verified")

        marker = "PRIVATE_TEST_" + uuid.uuid4().hex
        stub = directory / "stub.conf"
        stub.write_text(
            "map $arg_bad_id $fixture_request_id { default " + REQUEST_ID + "; 1 " + marker + "; }\n"
            "server { listen 8080; access_log off; error_log /dev/stderr crit; "
            "location / { add_header X-Request-ID $fixture_request_id; return 200 'ok'; } }\n"
        )
        for project in PROJECTS:
            project_root = ROOT.parent / project
            host = {
                "histae-dashboard": "dashboard.histae.com", "reverse-proxy": "api.histae.com",
                "histae-site": "www.histae.com",
            }[project]
            mounts = []
            for source, target in (
                (project_root / "nginx-logging.conf", "/etc/nginx/conf.d/00-logging.conf"),
                (project_root / "nginx.conf", "/etc/nginx/templates/default.conf.template"),
                (stub, "/etc/nginx/conf.d/zz-test-upstream.conf"),
            ):
                mounts.extend(["--mount", f"type=bind,src={source},dst={target},readonly"])
            options = [
                "--network", "none", "-e", "HISTAE_API_UPSTREAM=http://127.0.0.1:8080",
                "-e", "NGINX_ENVSUBST_FILTER=^HISTAE_API_UPSTREAM$", *mounts,
            ]
            docker("run", "--rm", *options, args.nginx_image, "nginx", "-t")
            name = "histae-edge-security-" + uuid.uuid4().hex[:12]
            try:
                docker("run", "-d", "--name", name, *options, args.nginx_image)
                ready = False
                for _ in range(30):
                    result = docker(
                        "exec", name, "wget", "-q", "-T", "1", "-O", "/dev/null",
                        "--header=Host: " + host, "http://127.0.0.1/", check=False,
                    )
                    if result.returncode == 0 or "404" in result.stderr:
                        ready = True
                        break
                    time.sleep(.1)
                assert ready, f"{project}: Nginx never became ready"
                paths = [
                    f"/api/auth/admob/webhook?custom_data={marker}&signature={marker}&key_id=42",
                    f"/api/admin/users/{marker}?bad_id=1&reason={marker}&search={marker}",
                    f"/{marker}?token={marker}",
                ]
                for path in paths:
                    docker(
                        "exec", name, "wget", "-q", "-T", "2", "-O", "/dev/null",
                        "--header=Host: " + host, "--header=Authorization: Bearer " + marker,
                        "--header=Cookie: histae_admin_session=" + marker,
                        "--header=Referer: https://example.invalid/" + marker,
                        "http://127.0.0.1" + path, check=False,
                    )
                logs = docker("logs", name)
                assert marker not in logs.stdout + logs.stderr, f"{project}: private marker logged"
                events = [json.loads(line) for line in logs.stdout.splitlines() if line.startswith("{")]
                assert len(events) >= 3, f"{project}: safe request logs missing"
                fields = {"time", "method", "route", "status", "duration", "request_id"}
                assert all(set(event) == fields for event in events), "Unexpected log fields"
                if project != "histae-site":
                    assert any(e["route"] == "admob-webhook" for e in events)
                    assert any(e["request_id"] == REQUEST_ID for e in events)
                    assert any(e["route"] == "api" and e["request_id"] == "" for e in events)
                print(f"{project}: nginx -t and private log redaction passed")
            finally:
                docker("rm", "-f", name, check=False)


if __name__ == "__main__":
    main()
