**FREE
// verify60: embedded SQL DELETE of a table another job holds *EXCL;
// SQLCODE and SQLSTATE written to GLD60O.
ctl-opt dftactgrp(*no);
dcl-f GLD60O usage(*output) rename(GLD60O:GLD60R);
exec sql set option commit = *none;
exec sql delete from AS4TRAINR1/GLD60F where K = 2;
res = 'SQL ' + %char(sqlcode) + ' ' + sqlstate;
write GLD60R;
*inlr = *on;
