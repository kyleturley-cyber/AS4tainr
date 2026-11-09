**FREE
// verify62: XML-INTO %HANDLER, 2 elements per call; then a handler that
// returns 1 after its first call. Results written to GLD62R.
ctl-opt dftactgrp(*no);
dcl-f GLD62R usage(*output) rename(GLD62R:GLD62RR);
dcl-ds itemT qualified template;
  name char(10);
  qty packed(5:0);
end-ds;
dcl-s total packed(7:0);
dcl-s calls int(10);
dcl-s names varchar(30);
dcl-s stopAt int(10);
dcl-s xml varchar(500);
xml = '<order><item><name>a</name><qty>2</qty></item><item><name>b</name>'
    + '<qty>3</qty></item><item><name>c</name><qty>4</qty></item></order>';
xml-into %handler(take : total) %xml(xml : 'path=order/item');
res = %char(total) + ' ' + %char(calls) + ' ' + names;
write GLD62RR;
total = 0;
calls = 0;
names = '';
stopAt = 1;
xml-into %handler(take : total) %xml(xml : 'path=order/item');
res = %char(total) + ' ' + %char(calls) + ' ' + names;
write GLD62RR;
*inlr = *on;
dcl-proc take;
  dcl-pi *n int(10);
    tot packed(7:0);
    items likeds(itemT) dim(2) const;
    n uns(10) value;
  end-pi;
  dcl-s i int(10);
  calls += 1;
  for i = 1 to n;
    tot += items(i).qty;
    names += %trim(items(i).name);
  endfor;
  if stopAt > 0 and calls >= stopAt;
    return 1;
  endif;
  return 0;
end-proc;
