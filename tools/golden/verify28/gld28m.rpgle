**FREE
// verify28: a module for a binder source with two *CURRENT blocks
ctl-opt nomain;
dcl-proc gld28one export;
  dcl-pi *n int(10) end-pi;
  return 1;
end-proc;
dcl-proc gld28two export;
  dcl-pi *n int(10) end-pi;
  return 2;
end-proc;
