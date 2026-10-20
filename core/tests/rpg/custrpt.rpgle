**FREE
// An externally described printer file (ILE RPG Reference: WRITE of a
// printer file's record format, OFLIND): the heading again when the
// overflow indicator comes on.
dcl-f custmast keyed;
dcl-f custrpt printer oflind(*in99);
dcl-s count packed(3:0);
write head;
read custmast;
dow not %eof(custmast);
  if *in99;
    write head;
    *in99 = *off;
  endif;
  *in50 = cusbal < 0;
  write detail;
  count += 1;
  read custmast;
enddo;
write total;
*inlr = *on;
