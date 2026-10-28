**FREE
// Golden (tools/golden/verify20.txt): an RPG program that calls another
// one (GLD20Q) and shows a DSPLY line, for the printed job log.
ctl-opt dftactgrp(*no) actgrp(*caller);
dcl-pr gld20q extpgm('GLD20Q') end-pr;
gld20q();
dsply 'GLD20P dsply';
*inlr = *on;
return;
