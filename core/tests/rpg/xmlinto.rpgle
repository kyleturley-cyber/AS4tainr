**FREE
dcl-ds addr qualified template;
  city varchar(20);
  zip char(5);
end-ds;
dcl-ds cust qualified;
  id packed(5:0);
  name varchar(30);
  since date;
  vip ind;
  phone char(12) dim(3);
  numphone int(10);
  home likeds(addr);
end-ds;
dcl-s names varchar(10) dim(5);
dcl-s x varchar(1000);
x = '<cust id="17"><name>  Ann   Lee </name><since>2020-02-29</since>'
  + '<vip>1</vip><phone>555-1</phone><phone>555-2</phone>'
  + '<home><city>Oslo</city><zip>0150</zip></home></cust>';
xml-into cust %xml(x: 'countprefix=num');
dsply (%char(cust.id) + '/' + cust.name + '/' + %char(cust.since) + '/' + cust.phone(2) + '/' + %char(cust.numphone) + '/' + cust.home.city);
x = '<list><names>a</names><names>b</names><names>c</names></list>';
xml-into names %xml(x);
dsply (names(1) + names(3));
x = '<Doc><Data><Names>q</Names></Data></Doc>';
xml-into names %xml(x: 'path=doc/data/names case=any');
dsply names(1);
monitor;
  xml-into cust %xml('<cust><bogus/></cust>');
on-error 353;
  dsply 'mismatch 353';
endmon;
xml-into(e) cust %xml('<cust');
dsply %char(%status);
*inlr = *on;
