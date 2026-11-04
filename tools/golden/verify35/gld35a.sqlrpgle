**FREE
// verify35: hexadecimal literals for integer parameters passed by value
// (XMLSERVICE: memset(p : x'40' : n), a 10I 0 VALUE parameter) and
// assigned to integer fields (XMLSERVICE: convPad = x'40').
ctl-opt dftactgrp(*no) actgrp(*new);
dcl-pr memset pointer extproc('__memset');
  p pointer value;
  c int(10) value;
  n uns(10) value;
end-pr;
dcl-s buf char(4) inz('abcd');
dcl-s i int(10);
dcl-s u uns(10);
dcl-s line varchar(200);

show('value 40: ' + %char(byval(x'40')));
show('value FFFF: ' + %char(byval(x'FFFF')));
memset(%addr(buf): x'40': 4);
if buf = *blanks;
  show('memset blanks');
else;
  show('memset not blanks: ' + %char(%len(%trim(buf))));
endif;
i = x'40';
show('eval int 40: ' + %char(i));
u = x'FFFFFFFF';
show('eval uns FFFFFFFF: ' + %char(u));
*inlr = *on;

dcl-proc byval;
  dcl-pi *n int(10);
    n int(10) value;
  end-pi;
  return n;
end-proc;

dcl-proc show;
  dcl-pi *n;
    text varchar(200) const;
  end-pi;
  line = text;
  exec sql insert into AS4TRAINR1/GLD35OUT (LINE) values(:line);
end-proc;
