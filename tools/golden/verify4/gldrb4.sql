-- Golden: COMMIT(*CHG): is data definition part of the unit of work
-- (a ROLLBACK after CREATE TABLE), and is the work rolled back when an
-- error stops the script (ERRLVL(10))?
create table AS4TRAINR1.GLDT5 (K int);
insert into AS4TRAINR1.GLDTO (ID, VAL) values ('RB4A', 'before rollback');
rollback;
insert into AS4TRAINR1.GLDTO (ID, VAL) values ('RB4B', 'before error');
insert into AS4TRAINR1.NOSUCHTBL values (1);
insert into AS4TRAINR1.GLDTO (ID, VAL) values ('RB4C', 'after error');
