**FREE
// Golden (tools/golden/verify14.txt): the events XML-SAX passes to a
// handler (event, length, exception ID, data) for two documents, each
// under MONITOR (verify13's GLD13X never ended on IBM i 7.5). Records go
// to AS4TRAINR1/GLD14OUT unblocked, so each one is there when it is
// written; the handler stops the parse after 40 events.
ctl-opt dftactgrp(*no) actgrp(*new);

dcl-f GLD14OUT usage(*output) rename(GLD14OUT: OUTR) block(*no);

dcl-ds psds psds;
  msgid char(7) pos(40);
end-ds;

dcl-s seq int(10) inz(0);
dcl-s doc varchar(200);

res('S1': 'start');
doc = '<a x="1">t</a>';
monitor;
  xml-sax %handler(saxh: seq) %xml(doc);
on-error;
  res('E1': %char(%status) + ' ' + msgid);
endmon;
res('S2': 'second');
seq = 100;
doc = '<?xml version="1.0"?><a x="1">t<b/><!-- c --><![CDATA[d]]>&amp;</a>';
monitor;
  xml-sax %handler(saxh: seq) %xml(doc);
on-error;
  res('E2': %char(%status) + ' ' + msgid);
endmon;
res('S3': 'end');

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
