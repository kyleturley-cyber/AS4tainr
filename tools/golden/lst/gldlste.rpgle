**FREE
// Golden compiler listing with errors (tools/golden/listings.txt): an
// undefined name and a type mismatch.
dcl-s count int(10);
dcl-s name char(10);

count = nosuch + 1;
name = count;
*inlr = *on;
