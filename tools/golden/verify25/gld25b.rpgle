**FREE
// verify25: U1, U2 and U8 as the program sees them; U2 on, U8 off at LR
dcl-pi *n;
  line char(60);
end-pi;
line = 'U1=0 U2=0 U8=0';
if *inu1;
  %subst(line : 4 : 1) = '1';
endif;
if *inu2;
  %subst(line : 9 : 1) = '1';
endif;
if *inu8;
  %subst(line : 14 : 1) = '1';
endif;
*inu2 = *on;
*inu8 = *off;
*inlr = *on;
