**FREE
// Golden (tools/golden/verify16.txt): CDATA sections in XML-SAX
// (verify14 and verify15 stopped at "<![" with exception 24): the bytes
// the compiler gave '[' and ']' (H1), then documents whose brackets are
// written as CCSID 37's X'BA' X'BB' (D1), as CCSID 273's X'63' X'FC'
// (D2) and as literals (D3); an unknown entity in an attribute (D4).
ctl-opt dftactgrp(*no) actgrp(*new);

dcl-f GLD16OUT usage(*output) rename(GLD16OUT: OUTR) block(*no);

dcl-ds psds psds;
  msgid char(7) pos(40);
end-ds;

dcl-s seq int(10) inz(0);
dcl-s doc varchar(200);
dcl-s br char(2) inz('[]');
dcl-s hx varchar(10);
dcl-s digits char(16) inz('0123456789ABCDEF');
dcl-s i int(10);
dcl-ds one;
  ch char(1);
  u uns(3) overlay(ch);
end-ds;

for i = 1 to 2;
  ch = %subst(br: i: 1);
  hx += %subst(digits: %div(u: 16) + 1: 1) + %subst(digits: %rem(u: 16) + 1: 1);
endfor;
res('H1': hx);

seq = 100;
res('D1': 'start');
doc = '<a><!' + x'BA' + 'CDATA' + x'BA' + 'd' + x'BBBB' + '></a>';
monitor;
  xml-sax %handler(saxh: seq) %xml(doc);
on-error;
  res('E1': %char(%status) + ' ' + msgid);
endmon;
seq = 200;
res('D2': 'start');
doc = '<a><!' + x'63' + 'CDATA' + x'63' + 'd' + x'FCFC' + '></a>';
monitor;
  xml-sax %handler(saxh: seq) %xml(doc);
on-error;
  res('E2': %char(%status) + ' ' + msgid);
endmon;
seq = 300;
res('D3': 'start');
doc = '<a><![CDATA[d]]></a>';
monitor;
  xml-sax %handler(saxh: seq) %xml(doc);
on-error;
  res('E3': %char(%status) + ' ' + msgid);
endmon;
seq = 400;
res('D4': 'start');
doc = '<a x="1&y;2"/>';
monitor;
  xml-sax %handler(saxh: seq) %xml(doc);
on-error;
  res('E4': %char(%status) + ' ' + msgid);
endmon;
res('S9': 'end');

*inlr = *on;

dcl-proc saxh;
  dcl-pi *n int(10);
    n int(10);
    event int(10) value;
    str pointer value;
    len int(20) value;
    exc int(10) value;
  end-pi;
  dcl-s text varchar(40);
  dcl-s l int(10);
  n += 1;
  text = '';
  if str <> *null and len > 0;
    l = len;
    if l > 40;
      l = 40;
    endif;
    text = %str(str: l);
  endif;
  res('X' + %editc(%dec(n: 4: 0): 'X'):
      %char(event) + ' ' + %char(len) + ' ' + %char(exc) + ' ' + text);
  if %rem(n: 100) >= 40;
    return -1;
  endif;
  return 0;
end-proc;

dcl-proc res;
  dcl-pi *n;
    id char(10) const;
    val varchar(200) const;
  end-pi;
  dcl-ds rec likerec(OUTR: *output);
  rec.ID = id;
  rec.VAL = val;
  write OUTR rec;
end-proc;
