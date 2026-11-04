//! Expected DSPFD TYPE(*MBR) reports for `mbrdesc.rs` tests: PUB400 (IBM i 7.5)
//! printed output of tools/golden/verify40.txt i40_mbr1 (2026-11-03) and
//! tools/golden/verify45.txt n45_mbra, n45_mbru, n45_mbrp, n45_mbrl, n45_mbrs
//! (2026-11-04), without PUB400's own page footer line and the exit status; the product
//! line and the processor name are as4tainr's (the tests do not compare them).

pub const I40_MBR1: &str = r#" AS4T001 V0R1M0  261021               Display File Description                                  26-11-03  17:27:21       Page    1
   File  . . . . . . . . . . . : GLD40T
     Library . . . . . . . . . : AS4TRAINR1
   Type of information . . . . : *MBR
   File attributes . . . . . . : *ALL
   System  . . . . . . . . . . : *LCL
   Processor . . . . . . . . . : AS4TAINR Display File Description Processor
 File . : GLD40T       Library . : AS4TRAINR1   Type of file . : Physical *DATA            Auxiliary Storage Pool ID . : 00001
                                        Member Description
   Member  . . . . . . . . . . . . . . . . . . : MBR        GLD40T
     Member level identifier . . . . . . . . . :            1261103172719
     Member creation date  . . . . . . . . . . :            26-11-03
     Text 'description'  . . . . . . . . . . . : TEXT
     Expiration date for member  . . . . . . . : EXPDATE    *NONE
     Access path maintenance . . . . . . . . . : MAINT      *IMMED
     Access path recovery  . . . . . . . . . . : RECOVER    *AFTIPL
     Member size . . . . . . . . . . . . . . . : SIZE       *NOMAX
     Current number of records . . . . . . . . :                     3
     Number of deleted records . . . . . . . . :                     1
     Allocate storage  . . . . . . . . . . . . : ALLOCATE   *NO
     Contiguous storage  . . . . . . . . . . . : CONTIG     *NO
     Preferred storage unit  . . . . . . . . . : UNIT       *ANY
     Records to force a write  . . . . . . . . : FRCRATIO   *NONE
     Share open data path  . . . . . . . . . . : SHARE      *NO
     Max % deleted records allowed . . . . . . : DLTPCT     *NONE
     Number of member accesses . . . . . . . . :                     0
     Data Space Activity Statistics  . . . . . :
       Data space size in bytes  . . . . . . . :              12288
       Physical file open accesses . . . . . . :                             2
       Physical file close accesses  . . . . . :                             2
       Write operations  . . . . . . . . . . . :                             4
       Update operations . . . . . . . . . . . :
       Delete operations . . . . . . . . . . . :                             1
       Logical Reads . . . . . . . . . . . . . :                             4
       Physical Reads  . . . . . . . . . . . . :
       Clear operations  . . . . . . . . . . . :
       Data space copy operations  . . . . . . :
       Reorganize operations . . . . . . . . . :
       Access paths builds/rebuilds  . . . . . :                             2
       Records rejected by key selection . . . :
       Records rejected by non-key selection . :
       Records rejected by group-by selection. :
     Access Path Activity Statistics . . . . . :
       Access path logical reads . . . . . . . :
       Access path physical reads  . . . . . . :
       Access path size  . . . . . . . . . . . :                     155648
       Number of access path entries . . . . . :                     3
       Access path valid . . . . . . . . . . . :            Yes
 AS4T001 V0R1M0  261021               Display File Description                                  26-11-03  17:27:21       Page    2
 File . : GLD40T       Library . : AS4TRAINR1   Type of file . : Physical *DATA            Auxiliary Storage Pool ID . : 00001
                                        Member Description
       Implicit access path sharing  . . . . . :            No
       Access path journaled . . . . . . . . . :            No
       Number of unique partial key values . . :
         Key field 1 . . . . . . . . . . . . . :                          3
     Last change date/time . . . . . . . . . . :            26-11-03  17:27:20
     Last save date/time . . . . . . . . . . . :
     Last restore date/time  . . . . . . . . . :
     Last used date  . . . . . . . . . . . . . :            26-11-03
     Days used count . . . . . . . . . . . . . :                1
       Reset date  . . . . . . . . . . . . . . :
     Object restored with partial transactions,
       apply journal changes required  . . . . :            *NO
     Partial transaction exists,
       roll back ended . . . . . . . . . . . . :            *NO
     Starting journal receiver for apply . . . :
       Library . . . . . . . . . . . . . . . . :
       ASP Device  . . . . . . . . . . . . . . :
     Constraint access path attributes . . . . :
       Constraint  . . . . . . . . . . . . . . : CST        Q_AS4TRAINR1_GLD40T_K_00001
         Access path recovery  . . . . . . . . :            *AFTIPL
         Forced key access path  . . . . . . . :            *NO
         Key must be unique  . . . . . . . . . :            *YES
         Access path journaled . . . . . . . . :            *NO
         Access path valid . . . . . . . . . . :            *YES
         Number of access path entries . . . . :                     3
         Access path size  . . . . . . . . . . :                     155648
         Access path logical reads . . . . . . :
         Access path physical reads  . . . . . :"#;

pub const N45_MBRA: &str = r#" AS4T001 V0R1M0  261021               Display File Description                                  26-11-04  22:55:55       Page    1
   File  . . . . . . . . . . . : GLD45A
     Library . . . . . . . . . : AS4TRAINR1
   Type of information . . . . : *MBR
   File attributes . . . . . . : *ALL
   System  . . . . . . . . . . : *LCL
   Processor . . . . . . . . . : AS4TAINR Display File Description Processor
 File . : GLD45A       Library . : AS4TRAINR1   Type of file . : Physical *DATA            Auxiliary Storage Pool ID . : 00001
                                        Member Description
   Member  . . . . . . . . . . . . . . . . . . : MBR        GLD45A
     Member level identifier . . . . . . . . . :            1261104225554
     Member creation date  . . . . . . . . . . :            26-11-04
     Text 'description'  . . . . . . . . . . . : TEXT
     Expiration date for member  . . . . . . . : EXPDATE    *NONE
     Member size . . . . . . . . . . . . . . . : SIZE       *NOMAX
     Current number of records . . . . . . . . :                     2
     Number of deleted records . . . . . . . . :                     0
     Allocate storage  . . . . . . . . . . . . : ALLOCATE   *NO
     Contiguous storage  . . . . . . . . . . . : CONTIG     *NO
     Preferred storage unit  . . . . . . . . . : UNIT       *ANY
     Records to force a write  . . . . . . . . : FRCRATIO   *NONE
     Share open data path  . . . . . . . . . . : SHARE      *NO
     Max % deleted records allowed . . . . . . : DLTPCT     *NONE
     Data Space Activity Statistics  . . . . . :
       Data space size in bytes  . . . . . . . :              12288
       Physical file open accesses . . . . . . :                             1
       Physical file close accesses  . . . . . :                             1
       Write operations  . . . . . . . . . . . :                             2
       Update operations . . . . . . . . . . . :
       Delete operations . . . . . . . . . . . :
       Logical Reads . . . . . . . . . . . . . :
       Physical Reads  . . . . . . . . . . . . :
       Clear operations  . . . . . . . . . . . :
       Data space copy operations  . . . . . . :
       Reorganize operations . . . . . . . . . :
       Access paths builds/rebuilds  . . . . . :
       Records rejected by key selection . . . :
       Records rejected by non-key selection . :
       Records rejected by group-by selection. :
     Access Path Activity Statistics . . . . . :
       Access path logical reads . . . . . . . :
       Access path physical reads  . . . . . . :
       Implicit access path sharing  . . . . . :            No
     Last change date/time . . . . . . . . . . :            26-11-04  22:55:54
     Last save date/time . . . . . . . . . . . :
     Last restore date/time  . . . . . . . . . :
     Last used date  . . . . . . . . . . . . . :            26-11-04
     Days used count . . . . . . . . . . . . . :                1
       Reset date  . . . . . . . . . . . . . . :
     Object restored with partial transactions,
       apply journal changes required  . . . . :            *NO
     Partial transaction exists,
       roll back ended . . . . . . . . . . . . :            *NO
     Starting journal receiver for apply . . . :
       Library . . . . . . . . . . . . . . . . :
       ASP Device  . . . . . . . . . . . . . . :"#;

pub const N45_MBRU: &str = r#" AS4T001 V0R1M0  261021               Display File Description                                  26-11-04  22:55:56       Page    1
   File  . . . . . . . . . . . : GLD45U
     Library . . . . . . . . . : AS4TRAINR1
   Type of information . . . . : *MBR
   File attributes . . . . . . : *ALL
   System  . . . . . . . . . . : *LCL
   Processor . . . . . . . . . : AS4TAINR Display File Description Processor
 File . : GLD45U       Library . : AS4TRAINR1   Type of file . : Physical *DATA            Auxiliary Storage Pool ID . : 00001
                                        Member Description
   Member  . . . . . . . . . . . . . . . . . . : MBR        GLD45U
     Member level identifier . . . . . . . . . :            1261104225555
     Member creation date  . . . . . . . . . . :            26-11-04
     Text 'description'  . . . . . . . . . . . : TEXT
     Expiration date for member  . . . . . . . : EXPDATE    *NONE
     Access path maintenance . . . . . . . . . : MAINT      *IMMED
     Access path recovery  . . . . . . . . . . : RECOVER    *AFTIPL
     Member size . . . . . . . . . . . . . . . : SIZE       *NOMAX
     Current number of records . . . . . . . . :                     3
     Number of deleted records . . . . . . . . :                     0
     Allocate storage  . . . . . . . . . . . . : ALLOCATE   *NO
     Contiguous storage  . . . . . . . . . . . : CONTIG     *NO
     Preferred storage unit  . . . . . . . . . : UNIT       *ANY
     Records to force a write  . . . . . . . . : FRCRATIO   *NONE
     Share open data path  . . . . . . . . . . : SHARE      *NO
     Max % deleted records allowed . . . . . . : DLTPCT     *NONE
     Number of member accesses . . . . . . . . :                     0
     Data Space Activity Statistics  . . . . . :
       Data space size in bytes  . . . . . . . :              12288
       Physical file open accesses . . . . . . :                             1
       Physical file close accesses  . . . . . :                             1
       Write operations  . . . . . . . . . . . :                             3
       Update operations . . . . . . . . . . . :
       Delete operations . . . . . . . . . . . :
       Logical Reads . . . . . . . . . . . . . :
       Physical Reads  . . . . . . . . . . . . :
       Clear operations  . . . . . . . . . . . :
       Data space copy operations  . . . . . . :
       Reorganize operations . . . . . . . . . :
       Access paths builds/rebuilds  . . . . . :                             2
       Records rejected by key selection . . . :
       Records rejected by non-key selection . :
       Records rejected by group-by selection. :
     Access Path Activity Statistics . . . . . :
       Access path logical reads . . . . . . . :
       Access path physical reads  . . . . . . :
       Access path size  . . . . . . . . . . . :                     155648
       Number of access path entries . . . . . :                     3
       Access path valid . . . . . . . . . . . :            Yes
 AS4T001 V0R1M0  261021               Display File Description                                  26-11-04  22:55:56       Page    2
 File . : GLD45U       Library . : AS4TRAINR1   Type of file . : Physical *DATA            Auxiliary Storage Pool ID . : 00001
                                        Member Description
       Implicit access path sharing  . . . . . :            No
       Access path journaled . . . . . . . . . :            No
       Number of unique partial key values . . :
         Key field 1 . . . . . . . . . . . . . :                          2
         Key fields 1 - 2  . . . . . . . . . . :                          3
     Last change date/time . . . . . . . . . . :            26-11-04  22:55:56
     Last save date/time . . . . . . . . . . . :
     Last restore date/time  . . . . . . . . . :
     Last used date  . . . . . . . . . . . . . :            26-11-04
     Days used count . . . . . . . . . . . . . :                1
       Reset date  . . . . . . . . . . . . . . :
     Object restored with partial transactions,
       apply journal changes required  . . . . :            *NO
     Partial transaction exists,
       roll back ended . . . . . . . . . . . . :            *NO
     Starting journal receiver for apply . . . :
       Library . . . . . . . . . . . . . . . . :
       ASP Device  . . . . . . . . . . . . . . :
     Constraint access path attributes . . . . :
       Constraint  . . . . . . . . . . . . . . : CST        GLD45UPK
         Access path recovery  . . . . . . . . :            *AFTIPL
         Forced key access path  . . . . . . . :            *NO
         Key must be unique  . . . . . . . . . :            *YES
         Access path journaled . . . . . . . . :            *NO
         Access path valid . . . . . . . . . . :            *YES
         Number of access path entries . . . . :                     3
         Access path size  . . . . . . . . . . :                     155648
         Access path logical reads . . . . . . :
         Access path physical reads  . . . . . :
       Constraint  . . . . . . . . . . . . . . : CST        GLD45UQ
         Access path recovery  . . . . . . . . :            *AFTIPL
         Forced key access path  . . . . . . . :            *NO
         Key must be unique  . . . . . . . . . :            *YES
         Access path journaled . . . . . . . . :            *NO
         Access path valid . . . . . . . . . . :            *YES
         Number of access path entries . . . . :                     3
         Access path size  . . . . . . . . . . :                     155648
         Access path logical reads . . . . . . :
         Access path physical reads  . . . . . :"#;

pub const N45_MBRP: &str = r#" AS4T001 V0R1M0  261021               Display File Description                                  26-11-04  22:56:01       Page    1
   File  . . . . . . . . . . . : GLD45P
     Library . . . . . . . . . : AS4TRAINR1
   Type of information . . . . : *MBR
   File attributes . . . . . . : *ALL
   System  . . . . . . . . . . : *LCL
   Processor . . . . . . . . . : AS4TAINR Display File Description Processor
 File . : GLD45P       Library . : AS4TRAINR1   Type of file . : Physical *DATA            Auxiliary Storage Pool ID . : 00001
                                        Member Description
   Member  . . . . . . . . . . . . . . . . . . : MBR        GLD45P
     Member level identifier . . . . . . . . . :            1261104225559
     Member creation date  . . . . . . . . . . :            26-11-04
     Text 'description'  . . . . . . . . . . . : TEXT
     Expiration date for member  . . . . . . . : EXPDATE    *NONE
     Access path maintenance . . . . . . . . . : MAINT      *IMMED
     Access path recovery  . . . . . . . . . . : RECOVER    *NO
     Member size                                 SIZE
       Initial number of records . . . . . . . :                 10000
       Increment number of records . . . . . . :             1000
       Maximum number of increments  . . . . . :                3
     Current number of increments  . . . . . . :                     0
     Record capacity . . . . . . . . . . . . . :                 13000
     Current number of records . . . . . . . . :                     2
     Number of deleted records . . . . . . . . :                     1
     Allocate storage  . . . . . . . . . . . . : ALLOCATE   *NO
     Contiguous storage  . . . . . . . . . . . : CONTIG     *NO
     Preferred storage unit  . . . . . . . . . : UNIT       *ANY
     Records to force a write  . . . . . . . . : FRCRATIO   *NONE
     Share open data path  . . . . . . . . . . : SHARE      *NO
     Max % deleted records allowed . . . . . . : DLTPCT     *NONE
     Number of member accesses . . . . . . . . :                     0
     Data Space Activity Statistics  . . . . . :
       Data space size in bytes  . . . . . . . :               8192
       Physical file open accesses . . . . . . :                             2
       Physical file close accesses  . . . . . :                             2
       Write operations  . . . . . . . . . . . :                             3
       Update operations . . . . . . . . . . . :
       Delete operations . . . . . . . . . . . :                             1
       Logical Reads . . . . . . . . . . . . . :
       Physical Reads  . . . . . . . . . . . . :
       Clear operations  . . . . . . . . . . . :
       Data space copy operations  . . . . . . :
       Reorganize operations . . . . . . . . . :
       Access paths builds/rebuilds  . . . . . :                             1
       Records rejected by key selection . . . :
       Records rejected by non-key selection . :
       Records rejected by group-by selection. :
     Access Path Activity Statistics . . . . . :
       Access path logical reads . . . . . . . :
       Access path physical reads  . . . . . . :
       Access path size  . . . . . . . . . . . :                      90112
       Number of access path entries . . . . . :                     2
       Access path valid . . . . . . . . . . . :            Yes
 AS4T001 V0R1M0  261021               Display File Description                                  26-11-04  22:56:01       Page    2
 File . : GLD45P       Library . : AS4TRAINR1   Type of file . : Physical *DATA            Auxiliary Storage Pool ID . : 00001
                                        Member Description
       Implicit access path sharing  . . . . . :            No
       Access path journaled . . . . . . . . . :            No
       Number of unique partial key values . . :
         Key field 1 . . . . . . . . . . . . . :                          2
     Last change date/time . . . . . . . . . . :            26-11-04  22:56:00
     Last save date/time . . . . . . . . . . . :
     Last restore date/time  . . . . . . . . . :
     Last used date  . . . . . . . . . . . . . :            26-11-04
     Days used count . . . . . . . . . . . . . :                1
       Reset date  . . . . . . . . . . . . . . :
     Object restored with partial transactions,
       apply journal changes required  . . . . :            *NO
     Partial transaction exists,
       roll back ended . . . . . . . . . . . . :            *NO
     Starting journal receiver for apply . . . :
       Library . . . . . . . . . . . . . . . . :
       ASP Device  . . . . . . . . . . . . . . :"#;

pub const N45_MBRL: &str = r#" AS4T001 V0R1M0  261021               Display File Description                                  26-11-04  22:56:03       Page    1
   File  . . . . . . . . . . . : GLD45L
     Library . . . . . . . . . : AS4TRAINR1
   Type of information . . . . : *MBR
   File attributes . . . . . . : *ALL
   System  . . . . . . . . . . : *LCL
   Processor . . . . . . . . . : AS4TAINR Display File Description Processor
 File . : GLD45L       Library . : AS4TRAINR1   Type of file . : Logical *DATA             Auxiliary Storage Pool ID . : 00001
                                        Member Description
   Member  . . . . . . . . . . . . . . . . . . : MBR        GLD45L
     Member level identifier . . . . . . . . . :            1261104225602
     Member creation date  . . . . . . . . . . :            26-11-04
     Text 'description'  . . . . . . . . . . . : TEXT
     Expiration date for member  . . . . . . . : EXPDATE    *NONE
     Access path maintenance . . . . . . . . . : MAINT      *IMMED
     Access path recovery  . . . . . . . . . . : RECOVER    *NO
     Preferred storage unit  . . . . . . . . . : UNIT       *ANY
     Record format selector program  . . . . . : FMTSLR     *NONE
     Records to force a write  . . . . . . . . : FRCRATIO   *NONE
     Share open data path  . . . . . . . . . . : SHARE      *NO
     Access Path Activity Statistics . . . . . :
       Access path logical reads . . . . . . . :
       Access path physical reads  . . . . . . :
       Index size  . . . . . . . . . . . . . . :                      90112
       Access path valid . . . . . . . . . . . :            Yes
       Implicit access path sharing  . . . . . :            No
       Access path journaled . . . . . . . . . :            No
       Number of unique partial key values . . :
         Key field 1 . . . . . . . . . . . . . :                          2
     Last change date/time . . . . . . . . . . :            26-11-04  22:56:02
     Last save date/time . . . . . . . . . . . :
     Last restore date/time  . . . . . . . . . :
     Last used date  . . . . . . . . . . . . . :
     Days used count . . . . . . . . . . . . . :                0
       Reset date  . . . . . . . . . . . . . . :
     Number of data members  . . . . . . . . . :                1
     Based on file . . . . . . . . . . . . . . :            GLD45P
       Library . . . . . . . . . . . . . . . . :            AS4TRAINR1
       Member  . . . . . . . . . . . . . . . . :            GLD45P
       Logical file format . . . . . . . . . . :            GLD45R
       Number of index entries . . . . . . . . :                     2"#;

pub const N45_MBRS: &str = r#" AS4T001 V0R1M0  261021               Display File Description                                  26-11-04  22:56:06       Page    1
   File  . . . . . . . . . . . : GLD45S
     Library . . . . . . . . . : AS4TRAINR1
   Type of information . . . . : *MBR
   File attributes . . . . . . : *ALL
   System  . . . . . . . . . . : *LCL
   Processor . . . . . . . . . : AS4TAINR Display File Description Processor
 File . : GLD45S       Library . : AS4TRAINR1   Type of file . : Physical *SRC             Auxiliary Storage Pool ID . : 00001
                                        Member Description
   Member  . . . . . . . . . . . . . . . . . . : MBR        GLD44C
     Member level identifier . . . . . . . . . :            1261104225604
     Member creation date  . . . . . . . . . . :            26-11-04
     Text 'description'  . . . . . . . . . . . : TEXT
     Expiration date for member  . . . . . . . : EXPDATE    *NONE
     Member size                                 SIZE
       Initial number of records . . . . . . . :                 10000
       Increment number of records . . . . . . :             1000
       Maximum number of increments  . . . . . :              499
     Current number of increments  . . . . . . :                     0
     Record capacity . . . . . . . . . . . . . :                509000
     Current number of records . . . . . . . . :                     6
     Number of deleted records . . . . . . . . :                     0
     Allocate storage  . . . . . . . . . . . . : ALLOCATE   *NO
     Contiguous storage  . . . . . . . . . . . : CONTIG     *NO
     Preferred storage unit  . . . . . . . . . : UNIT       *ANY
     Records to force a write  . . . . . . . . : FRCRATIO   *NONE
     Share open data path  . . . . . . . . . . : SHARE      *NO
     Max % deleted records allowed . . . . . . : DLTPCT     *NONE
     Data Space Activity Statistics  . . . . . :
       Data space size in bytes  . . . . . . . :               8192
       Physical file open accesses . . . . . . :                             2
       Physical file close accesses  . . . . . :                             2
       Write operations  . . . . . . . . . . . :                             6
       Update operations . . . . . . . . . . . :
       Delete operations . . . . . . . . . . . :
       Logical Reads . . . . . . . . . . . . . :
       Physical Reads  . . . . . . . . . . . . :
       Clear operations  . . . . . . . . . . . :
       Data space copy operations  . . . . . . :
       Reorganize operations . . . . . . . . . :
       Access paths builds/rebuilds  . . . . . :
       Records rejected by key selection . . . :
       Records rejected by non-key selection . :
       Records rejected by group-by selection. :
     Access Path Activity Statistics . . . . . :
       Access path logical reads . . . . . . . :
       Access path physical reads  . . . . . . :
 AS4T001 V0R1M0  261021               Display File Description                                  26-11-04  22:56:06       Page    2
 File . : GLD45S       Library . : AS4TRAINR1   Type of file . : Physical *SRC             Auxiliary Storage Pool ID . : 00001
                                        Member Description
       Implicit access path sharing  . . . . . :            No
     Source type . . . . . . . . . . . . . . . :
     Last change date/time . . . . . . . . . . :            26-11-04  22:56:04
     Last source update date/time  . . . . . . :            26-11-04  22:56:04
     Last save date/time . . . . . . . . . . . :
     Last restore date/time  . . . . . . . . . :
     Last used date  . . . . . . . . . . . . . :            26-11-04
     Days used count . . . . . . . . . . . . . :                1
       Reset date  . . . . . . . . . . . . . . :
     Object restored with partial transactions,
       apply journal changes required  . . . . :            *NO
     Partial transaction exists,
       roll back ended . . . . . . . . . . . . :            *NO
     Starting journal receiver for apply . . . :
       Library . . . . . . . . . . . . . . . . :
       ASP Device  . . . . . . . . . . . . . . :
   Member  . . . . . . . . . . . . . . . . . . : MBR        GLD44B
     Member level identifier . . . . . . . . . :            1261104225605
     Member creation date  . . . . . . . . . . :            26-11-04
     Text 'description'  . . . . . . . . . . . : TEXT
     Expiration date for member  . . . . . . . : EXPDATE    *NONE
     Member size                                 SIZE
       Initial number of records . . . . . . . :                 10000
       Increment number of records . . . . . . :             1000
       Maximum number of increments  . . . . . :              499
     Current number of increments  . . . . . . :                     0
     Record capacity . . . . . . . . . . . . . :                509000
     Current number of records . . . . . . . . :                     6
     Number of deleted records . . . . . . . . :                     0
     Allocate storage  . . . . . . . . . . . . : ALLOCATE   *NO
     Contiguous storage  . . . . . . . . . . . : CONTIG     *NO
     Preferred storage unit  . . . . . . . . . : UNIT       *ANY
     Records to force a write  . . . . . . . . : FRCRATIO   *NONE
     Share open data path  . . . . . . . . . . : SHARE      *NO
     Max % deleted records allowed . . . . . . : DLTPCT     *NONE
     Data Space Activity Statistics  . . . . . :
       Data space size in bytes  . . . . . . . :               8192
       Physical file open accesses . . . . . . :                             2
       Physical file close accesses  . . . . . :                             2
       Write operations  . . . . . . . . . . . :                             6
       Update operations . . . . . . . . . . . :
       Delete operations . . . . . . . . . . . :
       Logical Reads . . . . . . . . . . . . . :
       Physical Reads  . . . . . . . . . . . . :
       Clear operations  . . . . . . . . . . . :
       Data space copy operations  . . . . . . :
       Reorganize operations . . . . . . . . . :
       Access paths builds/rebuilds  . . . . . :
       Records rejected by key selection . . . :
       Records rejected by non-key selection . :
       Records rejected by group-by selection. :
     Access Path Activity Statistics . . . . . :
       Access path logical reads . . . . . . . :
       Access path physical reads  . . . . . . :
 AS4T001 V0R1M0  261021               Display File Description                                  26-11-04  22:56:06       Page    3
 File . : GLD45S       Library . : AS4TRAINR1   Type of file . : Physical *SRC             Auxiliary Storage Pool ID . : 00001
                                        Member Description
       Implicit access path sharing  . . . . . :            No
     Source type . . . . . . . . . . . . . . . :
     Last change date/time . . . . . . . . . . :            26-11-04  22:56:05
     Last source update date/time  . . . . . . :            26-11-04  22:56:05
     Last save date/time . . . . . . . . . . . :
     Last restore date/time  . . . . . . . . . :
     Last used date  . . . . . . . . . . . . . :            26-11-04
     Days used count . . . . . . . . . . . . . :                1
       Reset date  . . . . . . . . . . . . . . :
     Object restored with partial transactions,
       apply journal changes required  . . . . :            *NO
     Partial transaction exists,
       roll back ended . . . . . . . . . . . . :            *NO
     Starting journal receiver for apply . . . :
       Library . . . . . . . . . . . . . . . . :
       ASP Device  . . . . . . . . . . . . . . :"#;
