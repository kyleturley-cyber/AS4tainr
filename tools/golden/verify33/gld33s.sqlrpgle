**FREE
// verify33: what MONITOR's caller sees of a divide by zero (%STATUS,
// the status and exception ID in the PSDS) when it happens here, in a
// subprocedure, in a service program's procedure called by prototype
// and through a procedure pointer; one line each into AS4TRAINR1/GLD33OUT.
ctl-opt dftactgrp(*no) actgrp(*caller) bnddir('AS4TRAINR1/GLD33B');
dcl-ds psds psds qualified;
  status zoned(5) pos(11);
  exceptionId char(7) pos(40);
end-ds;
dcl-pr divz int(10) extproc('DIVZ');
  d int(10) value;
end-pr;
dcl-pr byPtr int(10) extproc(ptr);
  d int(10) value;
end-pr;
dcl-s ptr pointer(*proc);
dcl-s z int(10) inz(0);
dcl-s r int(10);
dcl-s n int(10) inz(0);

monitor;
  r = 10 / z;
on-error;
  note('here');
endmon;
monitor;
  r = local(0);
on-error;
  note('subprocedure');
endmon;
monitor;
  r = divz(0);
on-error;
  note('service program');
endmon;
ptr = %paddr(divz);
monitor;
  r = byPtr(0);
on-error;
  note('procedure pointer');
endmon;
monitor;
  r = 10 / z;
on-error *program;
  note('here again');
endmon;
*inlr = *on;

dcl-proc local;
  dcl-pi *n int(10);
    d int(10) value;
  end-pi;
  return 10 / d;
end-proc;

dcl-proc note;
  dcl-pi *n;
    what varchar(30) const;
  end-pi;
  dcl-s line varchar(200);
  n += 1;
  line = what + ' status=' + %char(%status()) + ' psds=' + %char(psds.status)
       + ' id=' + psds.exceptionId;
  exec sql INSERT INTO AS4TRAINR1.GLD33OUT VALUES (:n, :line);
end-proc;
