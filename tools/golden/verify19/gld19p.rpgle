**FREE
// Golden (tools/golden/verify19.txt): messages an RPG program sends
// with QMHSNDPM (to itself and to its caller), for the printed job log:
// the sending module, procedure and statement of an RPG program.
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
text = 'GLD19P info to self';
QMHSNDPM('': '': text: %len(text): '*INFO': '*': 0: key: err);
text = 'GLD19P info to caller';
QMHSNDPM('': '': text: %len(text): '*INFO': '*': 1: key: err);
text = 'GLD19P diag to caller';
QMHSNDPM('': '': text: %len(text): '*DIAG': '*': 1: key: err);
*inlr = *on;
return;
