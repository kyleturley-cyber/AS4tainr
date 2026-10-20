**FREE
// Files declared in procedures (ILE RPG Reference, "File
// Considerations"): opened when the procedure is called and closed when
// it returns, unless STATIC; their records go through data structures.
dsply custname(2);
dsply custname(7);
dsply %char(nextcust()) + ' ' + %char(nextcust());
dsply %char(firstcust()) + ' ' + %char(firstcust());
*inlr = *on;

dcl-proc custname;
  dcl-pi *n char(30);
    no packed(5:0) value;
  end-pi;
  dcl-f custmast keyed;
  dcl-ds rec likerec(custrec);
  chain no custmast rec;
  if %found(custmast);
    return rec.cusnam;
  endif;
  return '?';
end-proc;

dcl-proc nextcust;
  dcl-pi *n packed(5:0) end-pi;
  dcl-f custmast keyed static;
  dcl-ds rec likerec(custrec);
  read custmast rec;
  return rec.cusno;
end-proc;

dcl-proc firstcust;
  dcl-pi *n packed(5:0) end-pi;
  dcl-f custmast keyed;
  dcl-ds rec likerec(custrec);
  read custmast rec;
  return rec.cusno;
end-proc;
