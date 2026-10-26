-- Golden: COMMIT and ROLLBACK statements in RUNSQLSTM COMMIT(*NONE).
insert into AS4TRAINR1.GLDTO (ID, VAL) values ('RB1', 'before rollback');
rollback;
insert into AS4TRAINR1.GLDTO (ID, VAL) values ('RB2', 'after rollback');
commit;
