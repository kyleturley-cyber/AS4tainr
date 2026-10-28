**FREE
// Golden (tools/golden/verify21.txt): an escape message sent to '*'
// counter 1 from the main procedure, that is to the program's own
// entry procedure: what the CL caller gets, and the job log.
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
text = 'GLD21Q escape';
QMHSNDPM('CPF9898': 'QCPFMSG   *LIBL': text: %len(text): '*ESCAPE': '*': 1: key: err);
*inlr = *on;
return;
