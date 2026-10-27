-- Golden: statements verify2 saw fail only as SQL9010, run with
-- ERRLVL(30) so that the listing gives each one's message ID. GLDTMP is
-- versioned and not journaled here.
update AS4TRAINR1.GLDTMP set CMD = 'WRKACTJOB' where GRP = 'G1';
delete from AS4TRAINR1.GLDTMP where GRP = 'G1';
insert into AS4TRAINR1.GLDTMP values ('G3', 1, 'WRKSPLF');
insert into AS4TRAINR1.GLDTO (ID, VAL) select 'ASOF', CMD from AS4TRAINR1.GLDTMP for system_time as of (select min(SYS_START) from AS4TRAINR1.GLDTMP_HST);
insert into AS4TRAINR1.GLDTO (ID, VAL) select 'ASOFCOL', CMD from AS4TRAINR1.GLDTMP for system_time as of SYS_START;
insert into AS4TRAINR1.GLDTO (ID, VAL) select 'NOTTMP', VAL from AS4TRAINR1.GLDTO for system_time as of current timestamp;
insert into AS4TRAINR1.GLDTO (ID, VAL) select 'MIXED', 'X' from AS4TRAINR1/GLDTO, SYSIBM.SYSDUMMY1;
insert into AS4TRAINR1.GLDTO (ID, VAL) select 'CHAIN', case when exists (select 1 from AS4TRAINR1.GLDTMP_HST A, AS4TRAINR1.GLDTMP_HST B where A.AUDIT_OPER = 'I' and B.AUDIT_OPER = 'U' and A.SYS_END = B.SYS_START) then 'END=NEXT START' else 'NO MATCH' end from SYSIBM.SYSDUMMY1;
alter table AS4TRAINR1.GLDTMP drop versioning;
alter table AS4TRAINR1.GLDTMP drop versioning;
alter table AS4TRAINR1.GLDTMP add versioning use history table AS4TRAINR1.GLDTMP_HST on delete add extra row;
