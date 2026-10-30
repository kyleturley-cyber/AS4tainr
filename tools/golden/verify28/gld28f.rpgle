**FREE
// verify28: float literals and %CHAR of floats; EXTPGM of a variable
// holding 'LIB/PGM'
dcl-pi *n;
  line char(200);
end-pi;
dcl-pr callee extpgm(pgmName);
  t char(10);
end-pr;
dcl-s pgmName char(21) inz('AS4TRAINR1/GLD28X');
dcl-s f float(8);
dcl-s t char(10) inz('before');
f = 1.5E3;
line = %char(f);
f = -1.797693134862315E+308;
line = %trimr(line) + '|' + %char(f) + '|' + %char(%abs(f));
callee(t);
line = %trimr(line) + '|' + t;
*inlr = *on;
