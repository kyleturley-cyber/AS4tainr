**FREE
// Golden compiler listing (tools/golden/listings.txt): a program with an
// externally described file, a data structure, a procedure and a few
// fields, compiled with CRTBNDRPG's default listing options.
ctl-opt dftactgrp(*no) actgrp(*new);

dcl-f GLDLSTF usage(*output);

dcl-ds totals qualified;
  count int(10);
  amount packed(9: 2);
end-ds;

dcl-s i int(10);
dcl-s msg char(30);

for i = 1 to 3;
  ID = 'ROW' + %char(i);
  AMT = i * 1.5;
  write GLDLSTR;
  totals.count += 1;
  totals.amount += AMT;
endfor;
msg = describe(totals.count);
dsply msg;
*inlr = *on;

dcl-proc describe;
  dcl-pi *n char(30);
    n int(10) const;
  end-pi;
  return 'Rows written: ' + %char(n);
end-proc;
