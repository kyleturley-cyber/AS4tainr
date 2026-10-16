      * CUSTMNT: change a customer (display file CUSTMNTD).
      *
      * CUSTMAST is opened for update, so the CHAIN locks the customer's
      * record while the operator types the change (IBM i record locking).
      * Another job that CHAINs the same customer waits for the lock; if it
      * is not released in time, the error indicator 31 (positions 73-74) is
      * set and SELECT says the customer is in use. Enter UPDATEs the record
      * (which releases the lock); F12 releases it with UNLOCK.
     FCUSTMNTD  CF   E             WORKSTN
     FCUSTMAST  UF   E           K DISK
     C                   DOW       NOT *IN03
     C                   EXFMT     SELECT
     C                   EVAL      *IN32 = *OFF
     C                   IF        NOT *IN03
     C     CUSNUM        CHAIN     CUSTMAST                           3031
     C                   IF        NOT *IN30 AND NOT *IN31
     C                   EXFMT     CHANGE
     C                   IF        *IN03 OR *IN12
     C                   UNLOCK    CUSTMAST
     C                   EVAL      *IN12 = *OFF
     C                   ELSE
     C                   UPDATE    CUSTREC
     C                   EVAL      *IN32 = *ON
     C                   ENDIF
     C                   ENDIF
     C                   ENDIF
     C                   ENDDO
     C                   SETON                                        LR
