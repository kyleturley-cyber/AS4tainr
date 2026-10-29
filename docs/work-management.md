# Work management: jobs, batch, messages

Phase 3 starts here. Written from IBM's public work management documentation.

## Jobs

Every signed-on session and every submitted batch job is a **job** with a qualified name `number/user/name`:

| Job | Name | Subsystem | Type |
| --- | --- | --- | --- |
| A 5250 session | the display device, e.g. `QPADEV0001` | `QINTER` | `INT` |
| A job submitted with `SBMJOB` | `JOB(...)`, or `QDFTJOBD` for `JOB(*JOBD)` | `QBATCH` | `BCH` |

Job numbers are six digits, in order. Jobs are kept in the catalog (`as4tainr.jobs`), so every session sees every job. A job that has ended keeps its row and its job log (status `OUTQ`).

When the server starts, jobs that were still active when it stopped are marked as ended abnormally.

## Batch jobs

```
SBMJOB CMD(CALL PGM(CUSTRPT) PARM(L)) JOB(RPT)
```

- `CMD` is any command, written as you would type it. It is checked when you submit it, and a command with errors is never submitted.
- The job runs as the submitting user, with the submitter's current library (`CURLIB(*CURRENT)`) and library list (`INLLIBL(*CURRENT)`), or the ones you give.
- It goes on job queue `QBATCH` and gets message `CPC1221` (submitted). The `QBATCH` subsystem runs in the server process. It takes jobs off the queue first in, first out, one at a time (`AS4TAINR_QBATCH_MAXACT` raises that).
- What the command sends goes to the job's log: CL program messages, `DSPLY` lines from RPG, and errors. A display command in batch writes its list to the job log.
- When the job ends, the submitting user gets `CPF1241` (completed normally) or `CPF1240` (ended abnormally) on their message queue. The command entry screen says when a new message is waiting.

Batch programs run without a display: `EXFMT` in a batch job fails, as it does on IBM i. An RPG program in batch has a time limit (120 seconds).

## Commands

| Command | What it does |
| --- | --- |
| `SBMJOB CMD(...) JOB(...) JOBQ(QBATCH) CURLIB(...) INLLIBL(...)` | Submits a batch job |
| `DSPJOB` | The job's definition, status and run attributes, call stack, library list, activation groups and environment variables (OPTION `*DFNA`, `*STSA`, `*RUNA`, `*PGMSTK`, `*LIBL`, `*ACTGRP`, `*ENVVAR`; `*SELECT` and `*ALL` give them all), printed in IBM i 7.5's layout with OUTPUT(*PRINT) (spooled file QPDSPJOB; under PASE `system` written out), or listed on the display |
| `WRKACTJOB` | Active jobs by subsystem, with what each is doing: `CMD-WRKACTJOB`, `PGM-ORDENT`, status `RUN`, `DSPW` (waiting at the display) or `LCKW` (waiting for a record lock, see [record-locks.md](record-locks.md)). Option 5 shows the job log, and F5 refreshes |
| `WRKSBMJOB` | Jobs you submitted: `JOBQ` (waiting), `ACTIVE`, `OUTQ` (ended). Option 4 ends a job that has not started, option 5 shows its log, F5 refreshes |
| `DSPJOBLOG JOB(*)` or `JOB(number/user/name)` | A job's log: the commands it ran (`>`) and the messages they sent |
| `ENDJOB JOB(number/user/name)` | Ends a job that is still on the job queue |
| `SNDMSG MSG('...') TOUSR(user)` | Sends a message to a user's message queue (`*SYSOPR` goes to `QSYSOPR`) |
| `DSPMSG` | Shows your messages, newest first, and marks them as read |
| `RTVJOBA JOB(&j) NBR(&n) USER(&u) SWS(&s) DATFMT(&f) ...` | In CL programs: the job's name, number and user, and its attributes as DSPJOB shows them (with CHGJOB's changes): every return variable of the CL reference |
| `CHGJOB JOB(*) SWS(10XXXXX1) DATFMT(*YMD) INQMSGRPY(*DFT) LOG(...) ...` | Changes this job's attributes (`jobattr.rs`, kept in the process by job); values checked as the CL reference lists them (a value that is not valid: a diagnostic and CPF0006, nothing changed), *SYSVAL resolved when the command runs, X in SWS leaving a switch as it is; DEVRCYACN and TSEPOOL only for an interactive job (CPF1334), RUNPTY and the other run attributes only with *JOBCTL (CPF1337). DSPJOB and RTVJOBA show the changes. `%SWITCH(mask)` in CL tests the job switches; an RPG program's U1-U8 are the switches when it starts and become them when it ends with LR on |

They are on the F4 menu under "Job and message commands".

CHGJOB changes only this job: another job named is found (CPF1321 when it is not there) but not changed (its attributes live in its own process); JOBQ, SCDDATE and SCDTIME (a job on a job queue) are not changed (CPF1343). The attributes change what DSPJOB and RTVJOBA show, the job switches (%SWITCH, RPG U1-U8); the others (date format, inquiry reply, logging level...) do not change how the job runs yet (docs/fidelity-gaps.md).

## Not yet

- Ending an active job (`ENDJOB` of a running job, `*IMMED`/`*CNTRLD`), holding and releasing jobs or job queues, job priorities, scheduled jobs (`SCDDATE`), `WRKJOBQ`, `WRKSBS`.
- Other job queues and subsystems, subsystem descriptions, job descriptions (`JOBD` is always `QDFTJOBD`), routing entries.
- Job logs are kept in the catalog for every job. As on IBM i, a batch job's log becomes spooled file `QPJOBLOG` only when the job ends abnormally (or with `DSPJOBLOG OUTPUT(*PRINT)`, `SIGNOFF LOG(*LIST)`); see [spooled-files.md](spooled-files.md).
- Message replies (`*INQ` messages), break messages, `RCVMSG` from a message queue (CL programs receive from their program message queue, see cl-programs.md), the `QSYSOPR` operator's console. `DSPMSG` is a plain list.
- Object authority (next in Phase 3). Record locks: see [record-locks.md](record-locks.md).
