**FREE
// verify57: embedded SQL DELETE of a table another job holds *EXCL;
// SQLCODE and SQLSTATE written to GLD57O.
ctl-opt dftactgrp(*no);
dcl-f GLD57O usage(*output) rename(GLD57O:GLD57R);
exec sql set option commit = *none;
exec sql delete from AS4TRAINR1/GLD57F where K = 2;
res = 'SQL ' + %char(sqlcode) + ' ' + sqlstate;
write GLD57R;
*inlr = *on;
