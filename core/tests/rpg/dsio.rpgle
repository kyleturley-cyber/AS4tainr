**FREE
// Data structure I/O (ILE RPG Reference, CHAIN, UPDATE, %KDS, %FIELDS):
// a record read into a LIKEREC data structure leaves the program fields
// alone; UPDATE with %FIELDS changes only the fields named.
dcl-f custmast usage(*update) keyed;
dcl-ds cust likerec(custrec);
dcl-ds key qualified;
  no packed(5:0);
end-ds;
key.no = 1;
chain %kds(key) custmast cust;
dsply cust.cusnam;
dsply %char(cusno);
cusnam = 'Changed name';
cusbal = 99;
chain 1 custmast;
cusnam = 'New name';
cusbal = 1;
update custrec %fields(cusbal);
chain 1 custmast;
dsply cusnam;
dsply %char(cusbal);
*inlr = *on;
