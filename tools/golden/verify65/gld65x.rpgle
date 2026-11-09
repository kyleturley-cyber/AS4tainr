**FREE
// verify65: XML-INTO %HANDLER, 2 elements per call; the totals to GLD65R.
ctl-opt dftactgrp(*no);
dcl-f GLD65R usage(*output) rename(GLD65R:GLD65RR);
dcl-ds itemT qualified template;
  name char(10);
  qty packed(5:0);
end-ds;
dcl-s total packed(7:0);
dcl-s calls int(10);
dcl-s names varchar(30);
dcl-s xml varchar(500);
xml = '<order><item><name>a</name><qty>2</qty></item><item><name>b</name>'
    + '<qty>3</qty></item><item><name>c</name><qty>4</qty></item></order>';
xml-into %handler(take : total) %xml(xml : 'path=order/item');
res = %char(total) + ' ' + %char(calls) + ' ' + names;
write GLD65RR;
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
  return 0;
end-proc;
