# Deployment and release checks

The server currently uses one owner and one ordered log per document. A passing
Lean build proves the specified models; native OOXML execution, storage and
network behavior are checked by Rust tests and operational probes. None of these
checks alone certify an untested deployment or workload.

## Ownership and routing

Start with one server behind a TLS reverse proxy. Its WebSocket connection stays
on that server for the connection's lifetime. With one backend, session affinity
is unnecessary.

With multiple backends, route **by document identity**, including WebSocket
upgrades/reconnects and HTTP downloads/uploads. Every client of a document must
reach the same owner. A cookie or source-IP sticky session can send two users
editing the same document to different owners and is insufficient. Apply the
same document-name decoding policy as the server; do not build a second,
incompatible interpretation of encoded paths at the router.

There is no distributed ownership registry, fencing protocol, replicated log or
automatic failover in this repository. Independent server processes cannot share
one `--data` directory: its OS lease intentionally rejects the second writer.
Separate directories can silently create independent copies of the same logical
document if the router is wrong. Horizontal partitioning needs an external
document-to-owner mapping; move ownership only after stopping the previous writer.
Document listings and new-document upload ownership also need an explicit policy.
Do not treat adding a load balancer as active-active support.

Table structure edits (rows, columns, merges and reference-changing operations)
are sequenced by that owner. Overlapping concurrent merges can both be valid in
their original views: the first admitted edit commits; an incompatible later
edit is rejected and the submitting replica rebases its optimistic view. Not
every conflicting request succeeds. Stable cell/row/column IDs preserve intent
through structural edits. Text in a concurrently deleted DOCX row can remain in
its tombstoned, invisible cell story. UI integrations must surface refusals rather
than report every local optimistic edit as saved. Lean grid invariants,
multi-client PBT and native-file comparisons cover these rules.

These guarantees apply within the supported editing capabilities. Ragged and
nested DOCX tables support text editing, but their structural editing is not
implemented; PPTX table cell-content and structural editing are also unsupported.
Check the [implementation scope](design.md) against the intended documents. Passing the
conflict tests does not add those unsupported operations.

## Single-owner baseline

Build an identified revision with `cargo build --release --locked`. Keep the
binary revision, native submodule revision and configuration with the release.
For example, after provisioning private token/configuration files and storage:

```sh
jade-collab serve \
  --addr 127.0.0.1:8787 \
  --data /srv/jade/data \
  --fsync every \
  --checkpoint-every 101 \
  --tokens /etc/jade/tokens.json \
  --audit-log /var/log/jade/access.jsonl
```

The listener is HTTP/WS: the proxy provides HTTPS/WSS and forwards WebSocket
upgrades. Keep the listener reachable only through the intended boundary.
Configure the supervisor to restart failures, retain audit output and rotate
the audit file using the supported SIGHUP reopen. Provision certificates and
restrict permissions for data, tokens and audit files. Server access tokens are
required here; a server started without `--tokens` permits unauthenticated access.

`--fsync every` syncs each acknowledged journal batch; the default checkpoint
policy does not promise retention of all acknowledged edits across power loss.
Durability still depends on the actual filesystem/storage honoring sync and
rename semantics. The probe's process kill is not a power-cut experiment.

Set proxy connection/request limits and host memory/disk limits for the intended
workload. Existing per-connection queue limits are not a global memory or
connection cap. Define an edit acknowledgment latency budget, concurrency,
document-size range and recovery target before accepting performance results.

## Cold backup and recovery

1. Stop the listener and all processes/tasks that own the server. Confirm the
   writer has exited. Never remove a live `.lock` file to force another owner.
2. Copy the **whole** storage directory to an independent backup destination.
   Individual snapshots or journals alone are insufficient. Back up token/proxy
   configuration separately with appropriate permissions and access controls.
3. Restore the complete storage tree into a fresh directory, using the matching
   release binary/configuration. Start one owner against it. Older binaries may
   not understand newer journal operations; test rollback with a compatible
   backup rather than opening new state with an arbitrary older binary.
4. Authenticate, reopen each format, compare expected content, download/reopen
   native files and perform an acknowledged edit. Reconnect saved replicas and
   check pending-ticket handling. Replicas outside `--keep-tail` may require a
   fresh join; retain pending edits for application reconciliation.

Native writer/storage faults deliberately stop the affected document. Recover
from its healthy durable prefix before resuming clients. A successful TCP
connection alone does not establish document readiness. The operational probe
uses an authenticated document sync, not just an open port.

## Repeatable local/staging probe

The `operational_probe` integration test starts the actual CLI subprocess with
authentication, `--data` and `--fsync every`. The probe defaults to checkpoints
every 101 records; set `JADE_PROBE_CHECKPOINT_EVERY` to match the deployment.
Independent clients edit DOCX text,
different XLSX cells and PPTX notes concurrently. Every local edit must receive
exactly one acknowledgment with no refusal. At the end it verifies replica
convergence and the acknowledged text/values in exported native files. It kills
the process without a graceful save, resumes every client, compares exports
byte-for-byte, then exercises a cold backup copied into another directory. Fresh
replica IDs and post-restore acknowledged writes are checked too.

The short lifecycle test runs in ordinary CI. Both probes need `curl`; RSS
sampling uses `ps` and reports null if unavailable. Run the longer probe from the
repository root, with existing writable directories:

```sh
JADE_PROBE_CLIENTS_PER_FORMAT=4 \
JADE_PROBE_SECONDS=600 \
JADE_PROBE_INTERVAL_MS=100 \
JADE_PROBE_CHECKPOINT_EVERY=101 \
JADE_PROBE_DATA_PARENT=/srv/jade/probes \
JADE_PROBE_REPORT=/srv/jade/probe-report.json \
cargo test --release --workspace --test operational_probe --locked \
  -- --ignored --nocapture
```

Settings are bounded: 1–64 clients per format (three times that many total),
1–86,400 seconds, 1–60,000 ms between a client's acknowledged edits, and
1–1,000,000 records per checkpoint. Each
client has at most one edit awaiting a verdict. The duration excludes setup and
recovery; catch-up may extend it. The temporary data/backup trees are removed
afterward; the optional JSON report remains at the requested path. Use a data
parent on the intended storage medium, with room for three copies plus logs.
Set `JADE_PROBE_KEEP_DATA=1` to retain the printed data directory and startup logs
for investigation after success or failure. Otherwise the temporary trees are
removed. A report is first written with `status: load_completed_recovery_pending`;
only a successful recovery/backup/write check replaces it with `status: passed`.
To replay a retained grown state, set `JADE_PROBE_SEED_DATA_DIR` to its `data/`
directory. The probe requires and holds the source storage lease while copying
it to its own temporary directory; an active writer is refused. The source is
not edited. Seed directories must contain the three `probe.*` documents created
by this harness. The report records the seed path; edit counts cover only the
new workload, while log positions include the retained prefix.
The total startup-to-authenticated-sync deadline remains 30 seconds, including
the full snapshot transfer. A separate one-second connection cutoff would
incorrectly reject a large but otherwise timely snapshot and is regression-tested.

Before plain-note run maintenance, the grown 10-minute local PPTX workload
exceeded the 30-second startup target with the 101-record interval; profiling
showed repeated native notes replay over thousands of redundant runs.
Note edits now stage conservative coalescing of adjacent unformatted runs before
native input. This also applies when replaying an old journal, without editing
its records. Rich or unknown XML remains a barrier, so this optimization is not
a recovery-time guarantee for arbitrary imported notes.
The example deployment/probe commands use 101 records, matching the release
profile measured below. The earlier 10-record mitigation reduced future replay
work but increased checkpoint IO; after the native notes fix, the 101-record
profile gave lower p99 latency across all three formats in the measured
10-minute workload. The server's general default remains 1000; none of these
counts is a recovery-time guarantee for arbitrary workloads. Changing the
interval by itself cannot shorten an already persisted journal: recover that
prefix once and take a healthy checkpoint before expecting a shorter replay
window. Check retained state and exported files during maintenance; do not edit
journal records.

The report includes actual acknowledged-edit counts, per-format p50/p95/p99/max
client-to-ack latency (including local planning), sampled peak server RSS during
the edit load (excluding clients and recovery), and
startup-to-authenticated-readiness timings for the initial load (including a
seed's retained journal), crash recovery and backup restore. Failed acknowledgments,
inconsistent state/files, duplicate commits on resume or failed recovery fail
the test. Performance numbers are measurements to compare with your deployment
budget; the probe does not silently invent a latency or memory SLO.

This is a closed-loop, small-fixture workload. It does not test a remote proxy,
TLS termination, public traffic, large imported documents, arbitrary edit mixes,
disk exhaustion, a storage power cut, distributed failover or hours of sustained
operation unless explicitly run that way. RAM-backed data cannot establish
physical-disk performance. The sample command uses a release binary; debug-build
measurements are not release performance estimates.

Before a production release, require all three CI jobs on the exact deployed
commit, exercise representative documents/concurrency on the deployment's
storage/proxy, compare latency/RSS/recovery measurements with explicit budgets,
and complete an independently stored backup/restore rehearsal. Record the
revision, toolchain, storage medium, configuration, duration and results.

Recorded local examples from 2026-10-23 are available for the
[20-second disk-backed debug run](verification/2026-10-23-debug-disk.json) and
[120-second RAM-backed debug run](verification/2026-10-23-debug-ram.json).
These record their implementation revision and build/storage conditions. Both
preserved acknowledged edits and exact exports through both restores; they are
functional/recovery evidence, not release-build performance or a production SLO.

Release measurements of the conservative plain-note maintenance are recorded in
the [native-path comparison](verification/2026-10-23-release-notes-native.json)
and [retained-state process probe](verification/2026-10-23-release-notes-grown.json).
The former applies the same 32 prepends to a notes part containing 9,857 runs:
median native-path time was 158 ms before and 6 ms with maintenance, with equal
text and exact other package parts. This excludes CRDT planning and networking.
The latter replays the original retained journal that previously took about
41 seconds, without modifying its 13 source files. Initial authenticated readiness
took 1.87 seconds with checkpoints every 101 records. It preserves the 40,259
previously acknowledged positions, admits 10,855 further edits from 12 clients
over 120 seconds, and checks exact exports after both forced termination and
cold backup restore. PPTX acknowledgment p99 was 71 ms; server load RSS peaked
at 863 MiB. Both measurements use a release binary on a shared macOS workstation
with RAM-backed APFS; they do not establish remote or physical-disk performance.

The [failed earlier cadence run](verification/2026-10-23-notes-cadence-failure.json)
is retained separately. Its volatile maintenance counter changed native run
layout across recovery despite equal text. An ordinary regression and native
stateful histories now compare exact uninterrupted/restored archives, and the
implementation decides eligibility from persisted XML alone. The long process
probe keeps the same exact-export requirement; a faster but unequal export is
a failure.

The following release runs use four clients per format, 100 ms between each
client's acknowledged edits and `--fsync every`. Each row links its complete
measurements, revision and build/storage conditions. The 600-second runs start
from fresh fixtures on RAM-backed APFS; the disk row uses internal physical
APFS for data, journals and backup copies.

| Run | Checkpoint records | Acknowledged edits | DOCX p99 ms | XLSX p99 ms | PPTX p99 ms | Peak server MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| [Before notes fix, 600 s](verification/2026-10-23-release-before-notes-600s.json) | 10 | 43,498 | 257 | 86 | 2,070 | 480 |
| [After fix, 600 s](verification/2026-10-23-release-notes-600s-checkpoint10.json) | 10 | 41,508 | 491 | 163 | 364 | 961 |
| [After fix, 600 s](verification/2026-10-23-release-notes-600s-checkpoint101.json) | 101 | 56,142 | 233 | 80 | 169 | 480 |
| [After fix, disk, 120 s](verification/2026-10-23-release-notes-disk.json) | 101 | 12,974 | 36 | 25 | 40 | 71 |

All runs in this table passed acknowledged-content, replica convergence, exact
exports after both restores, fresh replica IDs and post-restore writes. The
101-record 600-second run reached authenticated readiness 2.62 seconds after
forced termination and 2.18 seconds after cold backup restore. This is a
closed-loop comparison on a shared workstation: faster formats submit more
edits and grow larger documents, and background load varies. It does not isolate
causation or establish deployment capacity. In particular, the fixed code with
10-record checkpoints improved PPTX while DOCX/XLSX tails and peak RSS worsened;
those results are retained rather than omitted.

The [physical-backup restore rehearsal](verification/2026-10-23-release-notes-physical-backup.json)
copies the stopped 600-second run's complete storage tree from RAM-backed APFS
onto internal physical APFS. All thirteen files (43,166,583 bytes) and directories
are synced, and file SHA-256 values match. A fresh process restores from a copy
of that physical backup, preserving all 56,142 prior acknowledged positions;
twelve clients add 93 further edits, reconnect after forced termination and cold
backup restore, and verify exact native exports and fresh post-restore writes.
Initial authenticated readiness took 2.98 seconds. The physical source's hashes
remain unchanged. This tests independent storage media on one host; it does not
simulate a storage power cut or loss of that host.

The [source revision's CI record](verification/2026-10-23-notes-ci.json) identifies
all three successful jobs, 819 regular Rust tests, six executable Lean oracle
comparisons and the audit of 3,587 production declarations. The notes maintenance
model has twelve theorems; parser/native execution remain the explicit Rust
verification boundaries described in the [proof documentation](../lean/README.md).
