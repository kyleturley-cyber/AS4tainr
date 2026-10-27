-- Golden: listing message IDs not captured yet, and EXISTS in other
-- places than verify3's CASE in a select list. GLDTO is journaled to
-- QSQJRN in AS4TRAINR1 (made by y_qsqjrn), as GLDT4 should be when
-- CREATE TABLE finds that journal.
insert into AS4TRAINR1.GLDTO (ID, VAL) values ('D1', 'to delete');
insert into AS4TRAINR1.GLDTO (ID, VAL) values ('D2', 'to delete');
delete from AS4TRAINR1.GLDTO where ID in ('D1', 'D2');
create table AS4TRAINR1.GLDT4 (K int not null primary key, V char(10));
create index AS4TRAINR1.GLDT4I on AS4TRAINR1.GLDT4 (V);
label on table AS4TRAINR1.GLDT4 is 'golden 4';
comment on table AS4TRAINR1.GLDT4 is 'golden 4 comment';
insert into AS4TRAINR1.GLDTO (ID, VAL) select 'WHERE', 'x' from SYSIBM.SYSDUMMY1 where exists (select 1 from AS4TRAINR1.GLDTO);
insert into AS4TRAINR1.GLDTO (ID, VAL) values ('VALUES', case when exists (select 1 from AS4TRAINR1.GLDTO) then 'Y' else 'N' end);
update AS4TRAINR1.GLDTO set VAL = case when exists (select 1 from AS4TRAINR1.GLDT4) then 'Y' else 'N' end where ID = 'WHERE';
insert into AS4TRAINR1.GLDTO (ID, VAL) select 'SELCASE', case when 1 in (select 1 from SYSIBM.SYSDUMMY1) then 'IN' else 'NOT' end from SYSIBM.SYSDUMMY1;
drop index AS4TRAINR1.GLDT4I;
