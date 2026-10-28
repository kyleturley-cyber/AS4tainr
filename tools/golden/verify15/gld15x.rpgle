**FREE
// Golden (tools/golden/verify15.txt): the events XML-SAX passes to a
// handler (event, length, exception ID, data) for small documents, one
// kind of event each (references, CDATA, processing instruction, the
// declaration, DOCTYPE, references in an attribute, an unknown entity, a
// document that is not well formed), each under MONITOR. Records go to
// AS4TRAINR1/GLD15OUT unblocked; the handler stops after 40 events.
ctl-opt dftactgrp(*no) actgrp(*new);

dcl-f GLD15OUT usage(*output) rename(GLD15OUT: OUTR) block(*no);

dcl-ds psds psds;
  msgid char(7) pos(40);
end-ds;

dcl-s seq int(10) inz(0);
dcl-s doc varchar(200);

seq = 100;
res('D1': 'start');
doc = '<a>p &amp; q &lt;&gt;&apos;&quot; &#65;&#x42;</a>';
monitor;
  xml-sax %handler(saxh: seq) %xml(doc);
on-error;
  res('E1': %char(%status) + ' ' + msgid);
endmon;
seq = 200;
res('D2': 'start');
doc = '<a><![CDATA[d]]></a>';
monitor;
  xml-sax %handler(saxh: seq) %xml(doc);
on-error;
  res('E2': %char(%status) + ' ' + msgid);
endmon;
seq = 300;
res('D3': 'start');
doc = '<?pi data?><a/>';
monitor;
  xml-sax %handler(saxh: seq) %xml(doc);
on-error;
  res('E3': %char(%status) + ' ' + msgid);
endmon;
seq = 400;
res('D4': 'start');
doc = '<?xml version="1.0" encoding="IBM-037" standalone="yes"?><a/>';
monitor;
  xml-sax %handler(saxh: seq) %xml(doc);
on-error;
  res('E4': %char(%status) + ' ' + msgid);
endmon;
seq = 500;
res('D5': 'start');
doc = '<!DOCTYPE a><a/>';
monitor;
  xml-sax %handler(saxh: seq) %xml(doc);
on-error;
  res('E5': %char(%status) + ' ' + msgid);
endmon;
seq = 600;
res('D6': 'start');
doc = '<a x="1&amp;2&#65;"/>';
monitor;
  xml-sax %handler(saxh: seq) %xml(doc);
on-error;
  res('E6': %char(%status) + ' ' + msgid);
endmon;
seq = 700;
res('D7': 'start');
doc = '<a>&x;</a>';
monitor;
  xml-sax %handler(saxh: seq) %xml(doc);
on-error;
  res('E7': %char(%status) + ' ' + msgid);
endmon;
seq = 800;
res('D8': 'start');
doc = '<a><b></a>';
monitor;
  xml-sax %handler(saxh: seq) %xml(doc);
on-error;
  res('E8': %char(%status) + ' ' + msgid);
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
