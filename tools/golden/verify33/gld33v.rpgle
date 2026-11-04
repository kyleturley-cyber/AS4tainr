**FREE
// verify33: a service program procedure that divides by zero and one
// that fails with an escape message of its own (no handlers here).
ctl-opt nomain;
dcl-proc divz export;
  dcl-pi *n int(10);
    d int(10) value;
  end-pi;
  return 10 / d;
end-proc;
