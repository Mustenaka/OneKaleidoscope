# Upstream schema versions

Fetched at: `2026-08-09T07:39:58Z`

All versions and source revisions below are exact. The JSON files under
`schemas/` are unmodified upstream snapshots.

## Codex app-server

- CLI: `codex-cli 0.147.0`
- npm package: `@openai/codex@0.147.0`
- Bundle mode: default/stable output without `--experimental`
- JSON files: `285`
- Reproduction commands on Windows PowerShell:

```powershell
npm.cmd install --global @openai/codex@0.147.0
codex.cmd app-server generate-json-schema --out schemas/codex
```

- Reproduction commands on macOS/Linux:

```sh
npm install --global @openai/codex@0.147.0
codex app-server generate-json-schema --out schemas/codex
```

## OpenCode

- CLI: `1.18.8`
- npm package: `opencode-ai@1.18.8`
- OpenAPI document: `3.1.0`
- JSON files: `1`
- Reproduction commands on Windows PowerShell, using two terminals:

```powershell
npm.cmd install --global opencode-ai@1.18.8
opencode.cmd serve --pure --hostname 127.0.0.1 --port 4096 --log-level ERROR
```

```powershell
New-Item -ItemType Directory -Force schemas/opencode | Out-Null
curl.exe --fail --silent --show-error --noproxy "*" --header "Accept: application/json" http://127.0.0.1:4096/doc --output schemas/opencode/openapi.json
```

- Reproduction commands on macOS/Linux, using two terminals:

```sh
npm install --global opencode-ai@1.18.8
opencode serve --pure --hostname 127.0.0.1 --port 4096 --log-level ERROR
```

```sh
mkdir -p schemas/opencode
curl --fail --silent --show-error --noproxy "*" --header "Accept: application/json" http://127.0.0.1:4096/doc --output schemas/opencode/openapi.json
```

## Agent Client Protocol

- Rust crate: `agent-client-protocol@1.3.0`
- Wire protocol: `v1`
- Schema artifact: `agent-client-protocol-json-schema-v1@1.18.0`
- Git tags: `v1.3.0`, `schema-v1.18.0`
- Commit: `48b2abf1ac750fece26e03e92e773ccbd4754f5d`
- Repository: `https://github.com/agentclientprotocol/agent-client-protocol.git`
- Source paths: `schema/v1/schema.json`, `schema/v1/meta.json`
- JSON files: `2`
- Tag verification command:

```text
git ls-remote --tags https://github.com/agentclientprotocol/agent-client-protocol.git 'refs/tags/v1.3.0^{}' 'refs/tags/schema-v1.18.0^{}'
```

Both peeled tags must resolve to `48b2abf1ac750fece26e03e92e773ccbd4754f5d`.

- Reproduction commands on Windows PowerShell:

```powershell
New-Item -ItemType Directory -Force schemas/acp | Out-Null
curl.exe --fail --location --silent --show-error https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/48b2abf1ac750fece26e03e92e773ccbd4754f5d/schema/v1/schema.json --output schemas/acp/schema.json
curl.exe --fail --location --silent --show-error https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/48b2abf1ac750fece26e03e92e773ccbd4754f5d/schema/v1/meta.json --output schemas/acp/meta.json
```

- Reproduction commands on macOS/Linux:

```sh
mkdir -p schemas/acp
curl --fail --location --silent --show-error https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/48b2abf1ac750fece26e03e92e773ccbd4754f5d/schema/v1/schema.json --output schemas/acp/schema.json
curl --fail --location --silent --show-error https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/48b2abf1ac750fece26e03e92e773ccbd4754f5d/schema/v1/meta.json --output schemas/acp/meta.json
```
