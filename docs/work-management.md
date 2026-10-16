# Work management: jobs, batch, messages

Phase 3 starts here. Written from IBM's public work management documentation.

## Jobs

Every signed-on session and every submitted batch job is a **job** with a qualified name `number/user/name`:

| Job | Name | Subsystem | Type |
| --- | --- | --- | --- |
| A 5250 session | the display device, e.g. `QPADEV0001` | `QINTER` | `INT` |
| A job submitted with `SBMJOB` | `JOB(...)`, or `QDFTJOBD` for `JOB(*JOBD)` | `QBATCH` | `BCH` |

Job numbers are six digits, in order. Jobs are kept in the catalog (`ibmish.jobs`), so every session sees every job. A job that has ended keeps its row and its job log (status `OUTQ`).

When the server starts, jobs that were still active when it stopped are marked as ended abnormally.

## Batch jobs

```
SBMJOB CMD(CALL PGM(CUSTRPT) PARM(L)) JOB(RPT)
```

- `CMD` is any command, written as you would type it. It is checked when you submit it, and a command with errors is never submitted.
- The job runs as the submitting user, with the submitter's current library (`CURLIB(*CURRENT)`) and library list (`INLLIBL(*CURRENT)`), or the ones you give.
- It goes on job queue `QBATCH` and gets message `CPC1221` (submitted). The `QBATCH` subsystem runs in the server process. It takes jobs off the queue first in, first out, one at a time (`IBMISH_QBATCH_MAXACT` raises that).
- What the command sends goes to the job's log: CL program messages, `DSPLY` lines from RPG, and errors. A display command in batch writes its list to the job log.
- When the job ends, the submitting user gets `CPF1241` (completed normally) or `CPF1240` (ended abnormally) on their message queue. The command entry screen says when a new message is waiting.

Batch programs run without a display: `EXFMT` in a batch job fails, as it does on IBM i. An RPG program in batch has a time limit (120 seconds).

## Commands

| Command | What it does |
| --- | --- |
| `SBMJOB CMD(...) JOB(...) JOBQ(QBATCH) CURLIB(...) INLLIBL(...)` | Submits a batch job |
| `WRKACTJOB` | Active jobs by subsystem, with what each is doing: `CMD-WRKACTJOB`, `PGM-ORDENT`, status `RUN`, `DSPW` (waiting at the display) or `LCKW` (waiting for a record lock, see [record-locks.md](record-locks.md)). Option 5 shows the job log, and F5 refreshes |
| `WRKSBMJOB` | Jobs you submitted: `JOBQ` (waiting), `ACTIVE`, `OUTQ` (ended). Option 4 ends a job that has not started, option 5 shows its log, F5 refreshes |
| `DSPJOBLOG JOB(*)` or `JOB(number/user/name)` | A job's log: the commands it ran (`>`) and the messages they sent |
| `ENDJOB JOB(number/user/name)` | Ends a job that is still on the job queue |
| `SNDMSG MSG('...') TOUSR(user)` | Sends a message to a user's message queue (`*SYSOPR` goes to `QSYSOPR`) |
| `DSPMSG` | Shows your messages, newest first, and marks them as read |
| `RTVJOBA JOB(&j) NBR(&n) USER(&u)` | In CL programs: the job's name, number and user |

They are on the F4 menu under "Job and message commands".

## Not yet

- Ending an active job (`ENDJOB` of a running job, `*IMMED`/`*CNTRLD`), holding and releasing jobs or job queues, job priorities, scheduled jobs (`SCDDATE`), `WRKJOBQ`, `WRKSBS`.
- Other job queues and subsystems, subsystem descriptions, job descriptions (`JOBD` is always `QDFTJOBD`), routing entries.
- Spooled files: job logs are kept in the catalog, not as `QPJOBLOG` spooled files, and there are no printer files or `WRKSPLF` yet.
- Message replies (`*INQ` messages), break messages, message files, `RCVMSG`, the `QSYSOPR` operator's console. `DSPMSG` is a plain list.
- Object authority (next in Phase 3). Record locks: see [record-locks.md](record-locks.md).
