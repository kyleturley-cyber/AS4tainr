      * CUSTUPD: change a balance with UPDATE, add a customer with WRITE,
      * then read both back. Exercises the numbers Jariko sends to as4tainer.
      * Parameter: customer number to change (5,0).
     FCUSTMAST  UF A E           K DISK
     D msg             S             52
     D toFind          S              5  0
     C     *entry        plist
     C                   parm                    toFind
      * UPDATE: add 10.25 to the balance (also crosses zero for customer 5)
     C     toFind        chain     CUSTMAST
     C                   if        %found
     C                   eval      CUSBAL = CUSBAL + 10.25
     C                   update    CUSTREC
     C                   endif
      * WRITE: a new customer with a negative balance
     C                   eval      CUSNO = 6
     C                   eval      CUSNAM = 'Northwind Parts'
     C                   eval      CUSCTY = 'Fargo'
     C                   eval      CUSBAL = -1234.56
     C                   write     CUSTREC
      * Read both back
     C     toFind        chain     CUSTMAST
     C                   eval      msg = %trim(CUSNAM) + ' ' + %char(CUSBAL)
     C     msg           dsply
     C     6             chain     CUSTMAST
     C                   eval      msg = %trim(CUSNAM) + ' ' + %char(CUSBAL)
     C     msg           dsply
     C                   seton                                        lr
