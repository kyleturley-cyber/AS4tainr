**FREE
// Golden (tools/golden/verify20.txt): a program called by another RPG
// program sends messages to '*' counter 1 and to *PGMBDY counter 1:
// does the first reach its own program entry procedure or its caller?
ctl-opt dftactgrp(*no) actgrp(*caller);
dcl-pr QMHSNDPM extpgm('QMHSNDPM');
  msgid char(7) const;
  msgf char(20) const;
  msgdta char(256) const options(*varsize);
  msgdtalen int(10) const;
  msgtype char(10) const;
  stkent char(10) const;
  stkcnt int(10) const;
  msgkey char(4);
  errcode char(16);
end-pr;
dcl-s key char(4);
dcl-s err char(16) inz(*allx'00');
dcl-s text varchar(80);
text = 'GLD20Q star 1';
QMHSNDPM('': '': text: %len(text): '*INFO': '*': 1: key: err);
text = 'GLD20Q pgmbdy 1';
QMHSNDPM('': '': text: %len(text): '*INFO': '*PGMBDY': 1: key: err);
*inlr = *on;
return;
