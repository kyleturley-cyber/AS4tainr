**FREE
// Golden values for the ILE RPG precision rules of numeric operations
// (tools/golden/rpg-prec.txt): the decimal places of quotients, products
// and sums of literals, fields and nested expressions, shown with %CHAR.
ctl-opt dftactgrp(*no) actgrp(*new);

dcl-f GLDOUT usage(*output) rename(GLDOUT: OUTR);

dcl-s p72 packed(7: 2) inz(10);
dcl-s i10 int(10) inz(10);
dcl-s p1510 packed(15: 10) inz(1);
dcl-s z92 zoned(9: 2) inz(1234.5);

res('P01': %char(100 / 3));
res('P02': %char(1000 / 3));
res('P03': %char(10.5 / 3));
res('P04': %char(p72 / 3));
res('P05': %char(p72 / 0.3));
res('P06': %char(i10 / 3));
res('P07': %char(1 / 3 / 3));
res('P08': %char(p72 * p72));
res('P09': %char(p72 + 0.005));
res('P10': %char(2 / 3 * 3));
res('P11': %char(%dec(10: 5: 0) / 3));
res('P12': %char(p1510 / 7));
res('P14': %char(1 / 3 + 1));
res('P15': %char(10 / 4));
res('P16': %char(z92 / 7));
res('P17': %char(z92 * 0.001));
res('P18': %char(-7 / 2));
res('P19': %char(1 / 7 * 7));

*inlr = *on;

dcl-proc res;
  dcl-pi *n;
    key char(10) const;
    v varchar(60) const;
  end-pi;
  ID = key;
  VAL = v;
  write OUTR;
end-proc;
