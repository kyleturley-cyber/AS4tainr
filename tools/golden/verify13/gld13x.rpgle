**FREE
// Golden (tools/golden/verify13.txt): the event numbers XML-SAX passes
// to a handler, with each event's length, exception ID and data.
ctl-opt dftactgrp(*no) actgrp(*new);

dcl-f GLD13OUT usage(*output) rename(GLD13OUT: OUTR);

dcl-s seq int(10) inz(0);
dcl-s doc varchar(200);

doc = '<?xml version="1.0"?><a x="1">t<b/><!-- c --><![CDATA[d]]>&amp;</a>';
xml-sax %handler(saxh: seq) %xml(doc);

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
  res('X' + %editc(%dec(n: 3: 0): 'X'):
      %char(event) + ' ' + %char(len) + ' ' + %char(exc) + ' ' + text);
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
