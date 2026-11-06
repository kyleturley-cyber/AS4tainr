**FREE
// verify55: /SET and /RESTORE of DATFMT and TIMFMT; the values go to
// GLD55O as text.
ctl-opt dftactgrp(*no);
dcl-f GLD55O usage(*output) rename(GLD55O:GLD55R);
/SET DATFMT(*MDY/) TIMFMT(*HMS:)
dcl-s d1 date;
dcl-s t1 time;
dcl-s d2 date(*eur);
/RESTORE DATFMT
dcl-s d3 date;
dcl-s t3 time;
/RESTORE TIMFMT
dcl-s t4 time;
d1 = %date('2026-11-05': *iso);
d2 = d1;
d3 = d1;
t1 = %time('13.45.00': *iso);
t3 = t1;
t4 = t1;
res = %char(d1) + ' ' + %char(t1) + ' ' + %char(d2) + ' ' + %char(d3)
    + ' ' + %char(t3) + ' ' + %char(t4);
write GLD55R;
*inlr = *on;
