# iroh 1.0 NAT traversal spike

`spike-iroh` measures whether an iroh 1.0 connection has selected a direct IP
path by the end of a fixed observation window. Relay connectivity keeps a run
alive, but it never counts as a direct success. Every dial attempt, including a
connection failure, is appended to JSONL so that the G0 denominator is not
silently reduced.

## CLI

```text
spike-iroh listen [--out <path>] [--label <str>]
spike-iroh dial <TICKET> --label <str> [--window-secs 30] [--out <path>]
spike-iroh summarize <results.jsonl>
```

`dial` exits with `0` after a connected run that ends on a direct IP path, `10`
after a connected run that still ends on relay, and `20` for an invalid ticket,
connection failure, or another error. Codes `10` and `20` are measurement
outcomes; do not discard their JSONL records.

## Windows build and smoke run

From the repository root:

```powershell
cargo build --release --package kaleido-spike-iroh
$bin = ".\target\release\spike-iroh.exe"
```

Keep the listener running in the first PowerShell terminal. Its output file is a
PC-side mirror and should be separate from the dial-side file. Omitting
`--label` makes each mirror record inherit the corresponding dial label:

```powershell
& $bin listen --out ".\listen-results.jsonl"
```

Use `listen --label <value>` only when an intentional listener-side label
override is needed.

The listener prints one `TICKET ...` line. Copy the value after `TICKET` exactly,
then run a dial probe in a second terminal:

```powershell
$bin = ".\target\release\spike-iroh.exe"
$ticket = "<base64url ticket copied from the listener>"
& $bin dial $ticket --label "lan" --window-secs 30 --out ".\dial-results.jsonl"
$LASTEXITCODE
```

The same listener can accept repeated probes. Keeping
`listen-results.jsonl` and `dial-results.jsonl` separate avoids concurrent
writes to one file and preserves both sides of each observation.
If the two files are later concatenated, `summarize` de-duplicates records that
have matching non-empty `run_id` values and prefers the dial-side record.

## Android cross-compilation with cargo-ndk

Prerequisites are a Rust toolchain, Android SDK Platform Tools, an Android NDK,
and `cargo-ndk`. This repository pins the Rust target in
`rust-toolchain.toml`; the following commands also make the setup explicit:

```powershell
rustup target add aarch64-linux-android
cargo install cargo-ndk --version 4.1.2 --locked
$env:ANDROID_NDK_HOME = "$env:LOCALAPPDATA\Android\Sdk\ndk\28.2.13676358"
cargo ndk --target arm64-v8a --platform 21 build --release --package kaleido-spike-iroh
```

The resulting executable is:

```text
target\aarch64-linux-android\release\spike-iroh
```

To exercise the task card's literal Cargo target command, let `cargo-ndk`
export its linker and C-toolchain settings in a fresh PowerShell terminal, then
invoke Cargo:

```powershell
$env:ANDROID_NDK_HOME = "$env:LOCALAPPDATA\Android\Sdk\ndk\28.2.13676358"
$env:CARGO_NDK_PLATFORM = "21"
$ndkEnvironment = cargo ndk-env --target arm64-v8a --platform 21 --powershell | Out-String
Invoke-Expression $ndkEnvironment
$env:_CARGO_NDK_LINK_CLANG = "$env:ANDROID_NDK_HOME\toolchains\llvm\prebuilt\windows-x86_64\bin\clang.exe"
$env:_CARGO_NDK_LINK_TARGET = "--target=aarch64-linux-android21"
cargo build --target aarch64-linux-android --release
```

The two `_CARGO_NDK_LINK_*` variables are required when using `cargo-ndk
4.1.2` this way: `ndk-env` exports the wrapper path but omits the wrapper's
internal target and Clang variables. `Out-String` is also intentional because
PowerShell's `Invoke-Expression` rejects the blank line emitted by `ndk-env`
when pipeline input is evaluated one line at a time.

Do not use a Linux linker for this target. iroh's default TLS backend pulls in
`ring`, whose C and assembly build also needs the NDK compiler and archiver
settings supplied by `cargo-ndk`.

## Push into Termux

Shared storage is not an executable location. Push to Downloads, then copy the
binary into Termux's private home directory before running it.

On Windows:

```powershell
$adb = "$env:LOCALAPPDATA\Android\Sdk\platform-tools\adb.exe"
& $adb devices
& $adb push ".\target\aarch64-linux-android\release\spike-iroh" "/sdcard/Download/spike-iroh"
```

In Termux, grant shared-storage access once if needed, then copy and mark the
binary executable:

```sh
termux-setup-storage
cp ~/storage/downloads/spike-iroh ~/spike-iroh
chmod 700 ~/spike-iroh
./spike-iroh --help
```

For this standalone Termux binary there is no Android JVM context. iroh's
default Android DNS resolver therefore catches the missing JNI context by panic
unwinding and falls back to Google's DNS resolvers. Keep Rust's default
`panic = "unwind"` behavior. Setting `panic = "abort"` makes that fallback
impossible and can abort while constructing the endpoint. A future embedded
Android application should instead call
`iroh::dns::install_android_jni_context` before constructing an endpoint.

## G0: twenty cellular runs

1. Start `listen` on the Windows PC connected to home broadband and retain its
   complete stdout plus `listen-results.jsonl`.
2. On the phone, turn **Wi-Fi off** and confirm that traffic uses 4G or 5G.
   A phone connected to the home Wi-Fi measures LAN behavior and is not a G0
   sample.
3. Copy the listener ticket into Termux and run exactly twenty dial attempts
   with a label beginning with `4g`. Reuse one dial-side output file so every
   success, relay-only result, and connection failure remains in the sample.

Example Termux loop:

```sh
TICKET='<base64url ticket copied from the Windows listener>'
RESULTS="./dial-results-$(date +%Y%m%d-%H%M%S).jsonl"
test ! -e "$RESULTS" || exit 1
i=1
while [ "$i" -le 20 ]; do
  ./spike-iroh dial "$TICKET" \
    --label "4g" \
    --window-secs 30 \
    --out "$RESULTS"
  code=$?
  printf 'run=%s exit=%s\n' "$i" "$code"
  i=$((i + 1))
done
```

Do not enable shell `set -e`: exit `10` is an expected relay-only sample and
exit `20` must also remain in the twenty-run denominator. Retrieve both JSONL
files after the run. The dial-side file is authoritative for the G0 verdict
because it contains all twenty attempts. The PC listener can mirror connected
runs, but it cannot observe an attempt that never reaches the PC, so it must
never replace the dial-side denominator.

Always start a campaign with a new output filename. After summarizing, verify
that the `4g` row says exactly `runs=20`; do not mix older samples into the G0
decision.

Summarize the dial-side measurements on Windows:

```powershell
.\target\release\spike-iroh.exe summarize .\dial-results.jsonl
```

Summary rows are grouped by the exact label. Only labels beginning with `4g`
contribute to the final G0 verdict. The direct rate is:

```text
runs that connected and ended with a selected IP path / all runs
```

Consequently, relay-only and failed connection records stay in the denominator.
Exactly `60.0%` is `>= 60.0%` and keeps L2 relay optional; anything below
`60.0%` makes L2 relay mandatory for v1.

`median_time_to_direct` uses `direct_path_selected_ms`, not the earlier
`direct_path_opened_ms`. Null values are excluded. After sorting the remaining
values, an odd sample count uses the middle value; an even sample count uses
the arithmetic mean of the two middle values. The result is displayed in
seconds.
