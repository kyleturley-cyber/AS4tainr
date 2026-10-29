**FREE
// verify24: U1-U8 from the job switches; U2 on, U8 off at LR
dcl-s line char(60);
line = 'U1=' + %char(*inu1) + ' U2=' + %char(*inu2) + ' U8=' + %char(*inu8);
dsply line;
*inu2 = *on;
*inu8 = *off;
*inlr = *on;
