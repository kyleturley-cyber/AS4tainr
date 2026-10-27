-- Golden: COMMIT and ROLLBACK under commitment control (COMMIT(*CHG));
-- GLDTO is journaled for it.
insert into AS4TRAINR1.GLDTO (ID, VAL) values ('CMT1', 'rolled back');
rollback;
insert into AS4TRAINR1.GLDTO (ID, VAL) values ('CMT2', 'committed');
commit;
insert into AS4TRAINR1.GLDTO (ID, VAL) values ('CMT3', 'at the end');
