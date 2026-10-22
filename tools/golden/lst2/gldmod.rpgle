**FREE
// Golden listing of a module (tools/golden/listings2.txt): no main
// procedure, one exported procedure, a copy member.
ctl-opt nomain;
/copy GLDLSRC,GLDCPY

dcl-proc gldmsg export;
  dcl-pi *n char(40);
    name char(10) const;
  end-pi;
  dcl-s text char(40);
  text = GREETING + ' ' + %trim(name);
  return text;
end-proc;
