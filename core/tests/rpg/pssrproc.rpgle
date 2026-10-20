**FREE
// *PSSR in subprocedures (ILE RPG Reference, "Program Exception/Error
// Subroutine"; ILE RPG Programmer's Guide, "Using a Program Error
// Subroutine"): RETURN from it gives the procedure's value; reaching
// ENDSR without RETURN or GOTO, the procedure fails. CALLP(E) sets
// %ERROR and %STATUS.
dcl-s r int(10);
dsply %char(divide(10:0));
dsply %char(divide(10:5));
monitor;
  r = fails(1:0);
on-error;
  dsply 'fails failed';
endmon;
callp(e) fails(1:0);
dsply %char(%error) + ' ' + %char(%status);
*inlr = *on;

dcl-proc divide;
  dcl-pi *n int(10);
    a int(10) value;
    b int(10) value;
  end-pi;
  return a / b;
  begsr *pssr;
    return -1;
  endsr;
end-proc;

dcl-proc fails;
  dcl-pi *n int(10);
    a int(10) value;
    b int(10) value;
  end-pi;
  return a / b;
  begsr *pssr;
    dsply 'in pssr';
  endsr;
end-proc;
