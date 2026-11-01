**FREE
// verify34: a service program (ACTGRP(*CALLER)) with a procedure that
// divides by zero and one that ends normally.
ctl-opt nomain;
dcl-proc divz export;
  dcl-pi *n int(10);
    d int(10) value;
  end-pi;
  return 10 / d;
end-proc;
dcl-proc okay export;
  dcl-pi *n ind;
  end-pi;
  return *off;
end-proc;
