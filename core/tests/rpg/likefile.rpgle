**FREE
// LIKEFILE, TEMPLATE, file parameters, EXTFILE (ILE RPG Reference).
ctl-opt dftactgrp(*no);
dcl-f custmast template keyed;
dcl-f cust1 likefile(custmast);
dcl-f cust2 likefile(custmast) usropn extfile(fname);
dcl-ds rec likerec(custmast.custrec);
dcl-s fname char(21);
dcl-s n int(10);
n = countAll(cust1);
dsply ('count ' + %char(n));
fname = 'CUSTCOPY';
open cust2;
n = countAll(cust2);
dsply ('copy ' + %char(n));
setll *hival cust2;
readp cust2 rec;
dsply ('last ' + %char(rec.cusno));
close cust2;
*inlr = *on;

dcl-proc countAll;
  dcl-pi *n int(10);
    f likefile(custmast);
  end-pi;
  dcl-ds r likerec(custmast.custrec);
  dcl-s c int(10);
  setll *loval f;
  read f r;
  dow not %eof(f);
    c += 1;
    read f r;
  enddo;
  return c;
end-proc;
