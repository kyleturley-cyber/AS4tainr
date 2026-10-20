# Record locks

Two people changing the same customer at the same time must not overwrite each other's work. IBM i prevents this with record locks, and as4tainr implements the same rules for RPG programs. They are written from IBM's public documentation of record-level access.

## The rules

- A file the program opens for **update** (file type `U` on the F spec, or `USAGE(*UPDATE)`) locks every record it reads: `CHAIN`, `READ`, `READE`, `READP`, `READPE`.
- The lock lasts until the program does one of these with that file: `UPDATE` or `DELETE`, reads another record, `UNLOCK`, or closes the file (which includes the program ending).
- A read with the **N extender** (`CHAIN(N)`, `READ(N)`, ...) does not lock. Neither does a read of a file opened for input only.
- When another job holds the record, the read **waits** for it. If the lock is released in time, the read goes on with the record as the other job left it. If not, the read fails with `CPF5027`. With an error indicator (positions 73-74) or the `E` extender, the indicator comes on and the program goes on. Without one, the program ends with the error.
- Locks belong to the record of the **physical file**: a logical file over it sees the same locks.
- While a job waits for a lock, `WRKACTJOB` shows it with status `LCKW`.

How long a read waits is `AS4TAINR_WAITRCD` (seconds, default 60). On IBM i each file has its own `WAITRCD`; as4tainr uses one wait for every file.

## Example

`samples/qrpglesrc/CUSTMNT.rpgle` (with `CUSTMNTD.dspf`) changes a customer. CUSTMAST is opened for update, so the `CHAIN` locks the customer while the operator types:

```
     FCUSTMAST  UF   E           K DISK
     C     CUSNUM        CHAIN     CUSTMAST                           3031
     C                   IF        NOT *IN30 AND NOT *IN31
     C                   EXFMT     CHANGE
     C                   IF        *IN03 OR *IN12
     C                   UNLOCK    CUSTMAST
     C                   ELSE
     C                   UPDATE    CUSTREC
```

Sign on twice and `CALL CUSTMNT` in both sessions. When the second session asks for the same customer it waits. When the first presses Enter, the second gets the record with the first session's change. If the first takes too long, the second gets "Customer is being changed in another job" (indicator 31).

## How it works

- **Locks:** the record lock is a PostgreSQL advisory lock on the job's database session. The key is the physical file's name and the record number. The wait is the session's `lock_timeout`. The session's `application_name` is the job name, which is how `CPF5027` names the job that holds the record.
- **The RPG engine:** a file opened for update (`U` in position 17) locks each record it reads, unless the read has the `N` extender. `UPDATE`, `DELETE`, the next read, `UNLOCK` and closing the file release it. An error indicator in positions 73-74 (or `(E)`) catches CPF5027 instead of ending the program. Each program call has its own database session, named after the job (`runtime::job_session`).

## Not yet

- SQL statements (`STRSQL`, `RUNSQL`) and CL commands do not wait for record locks, and they do not take any.
- In free-form RPG, the `N` and `E` extenders and `UNLOCK` are not seen yet: free-form reads of update files always lock and never set an error indicator.
- Commitment control (locks held to `COMMIT`), `WAITRCD` per file (OVRDBF WAITRCD is accepted and ignored), and `DSPRCDLCK`/`WRKOBJLCK`.
