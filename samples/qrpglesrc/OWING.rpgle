      * OWING: read CUSTMAST through two logical files.
      * CUSTOWE  selects customers with a balance due (CUSBAL > 0) and is
      *          keyed by CUSBAL descending: largest balance first.
      * CUSTNAME has every customer, keyed by name: SETLL to the parameter,
      *          then READ two records.
      * Parameter: where to start in name order (1 character).
     FCUSTOWE   IF   E           K DISK
     FCUSTNAME  IF   E           K DISK
     D msg             S             52
     D from            S              1
     D n               S              3  0
     C     *entry        plist
     C                   parm                    from
     C                   read      CUSTOWE
     C                   dow       not %eof
     C                   eval      msg = %trim(CUSNAM) + ' ' + %char(CUSBAL)
     C     msg           dsply
     C                   read      CUSTOWE
     C                   enddo
     C     from          setll     CUSTNAME
     C                   eval      n = 0
     C                   read      CUSTNAME
     C                   dow       not %eof and n < 2
     C                   eval      msg = %trim(CUSNAM)
     C     msg           dsply
     C                   eval      n = n + 1
     C                   read      CUSTNAME
     C                   enddo
     C                   seton                                        lr
