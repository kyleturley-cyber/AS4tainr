-- Golden: a script without errors (listing and completion message).
insert into AS4TRAINR1.GLDTO (ID, VAL) values ('OK1', 'script ran');
update AS4TRAINR1.GLDTO set VAL = 'script ran again' where ID = 'OK1';
delete from AS4TRAINR1.GLDTO where ID = 'NONE';
create view AS4TRAINR1.GLDTOV as select ID, VAL from AS4TRAINR1.GLDTO;
drop view AS4TRAINR1.GLDTOV;
