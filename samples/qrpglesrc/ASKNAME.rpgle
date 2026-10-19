**FREE
// ASKNAME: DSPLY with a response. In an interactive job the message is
// shown with a reply line; the reply goes into the response field.
dcl-s name char(10);
dsply 'What is your name?' '' name;
dsply ('Hello, ' + %trim(name));
*inlr = *on;
