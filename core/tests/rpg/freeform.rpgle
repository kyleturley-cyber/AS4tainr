**FREE
// Free-form features of IBM i 7.3-7.5 and the built-in functions
// (ILE RPG Reference): each DSPLY shows a value the reference describes.
dcl-s d date inz(d'2026-01-31');
dcl-s d2 date;
dcl-s t time inz(t'23.30.00');
dcl-s ts timestamp inz(z'2026-10-20-13.45.30.000000');
dcl-s n packed(7:2) inz(1234.50);
dcl-s i int(10);
dcl-s s varchar(50);
dcl-s c char(20);
dcl-s words varchar(10) dim(5);
dcl-s auto char(3) dim(*auto : 10);
dcl-s nums packed(5:0) dim(4) inz(0);
dcl-s found ind;
dcl-s item varchar(10);

dcl-enum color qualified;
  red 'R';
  green 'G';
  blue 'B';
end-enum;

dcl-pr fmt varchar(30) overload(fmtNum : fmtDate);
dcl-pr fmtNum varchar(30);
  v packed(9:2) const;
end-pr;
dcl-pr fmtDate varchar(30);
  v date const;
end-pr;

// Dates: adding a month to January 31 gives the last day of February.
d2 = d + %months(1);
dsply %char(d2);
dsply %char(d2 : *usa);
dsply %char(%diff(d2 : d : *days));
dsply %char(%subdt(d : *years));
dsply %char(t + %minutes(45));
dsply %char(ts + %hours(12));
dsply %char(%date('10/20/26' : *mdy));
dsply %char(%date(20261020));
dsply %char(%dec(d2 : *iso));
dsply %char(%diff(ts : %timestamp(d) : *months));

// Lists, %SPLIT, FOR-EACH, IN.
words = %split('the quick  brown fox');
dsply %char(%elem(words)) + ' ' + words(3);
for-each item in %split('a,b,c' : ',');
  dsply item;
endfor;
if 5 in %range(1 : 10);
  dsply 'in range';
endif;
if 'G' in color;
  dsply 'green is a color';
endif;
if not ('X' in %list('A' : 'B'));
  dsply 'X not listed';
endif;
dsply color.blue;

// A varying-dimension array.
auto(*next) = 'AAA';
auto(*next) = 'BBB';
dsply %char(%elem(auto));
for-each item in auto;
  dsply item;
endfor;

// Array built-ins.
nums(1) = 30;
nums(2) = 10;
nums(3) = 40;
nums(4) = 20;
dsply %char(%xfoot(nums));
dsply %char(%lookup(40 : nums));
dsply %char(%maxarr(nums));
dsply %char(%minarr(nums));

// Strings.
dsply %char(%scanr('a' : 'banana'));
dsply %scanrpl('an' : 'AN' : 'banana');
dsply %left('abcdef' : 2) + %right('abcdef' : 2);
dsply %concat(', ' : 'x' : 'y' : 'z');
dsply %char(%max(3 : 9 : 4)) + %char(%min(3 : 9 : 4));
dsply %editw(n : '  ,   .  ');
dsply %char(%sqrt(16));

// OVERLOAD: the candidate that takes the argument.
dsply fmt(12.5);
dsply fmt(d);

// SND-MSG *ESCAPE and ON-EXCP.
monitor;
  snd-msg *escape %msg('CPF9898' : 'QCPFMSG' : 'stop here');
on-excp 'CPF9898';
  dsply 'caught CPF9898';
endmon;

// TEST(DE): February 30 is not a date.
c = '2026-02-30';
test(de) *iso c;
if %error;
  dsply 'not a date';
endif;
test(de) *mdy0 i;
i = 102026;
test(de) *mdy0 i;
if not %error;
  dsply 'a date';
endif;

// SELECT with an operand.
select color.green;
  when-is color.red;
    dsply 'red';
  when-in %list('B' : 'G');
    dsply 'blue or green';
  other;
    dsply 'other';
endsl;

*inlr = *on;

dcl-proc fmtNum;
  dcl-pi *n varchar(30);
    v packed(9:2) const;
  end-pi;
  return 'number ' + %char(v);
end-proc;

dcl-proc fmtDate;
  dcl-pi *n varchar(30);
    v date const;
  end-pi;
  return 'date ' + %char(v : *eur);
end-proc;
