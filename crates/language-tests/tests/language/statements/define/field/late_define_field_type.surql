/**
[test]

[[test.results]]
value = "[{ a: 1, b: '2', id: table:1 }]"

[[test.results]]
value = "NONE"

[[test.results]]
value = "NONE"

[[test.results]]
value = "NONE"

[[test.results]]
value = "[{ a: 1, b: '2', id: table:1 }]"

[[test.results]]
error = "Found '1' for field `a`, with record `table:2`, but expected a number"

*/


CREATE table:1 SET a = 1, b = "2";

DEFINE TABLE OVERWRITE table SCHEMAFULL;
DEFINE FIELD a ON TABLE table TYPE number;
DEFINE FIELD b ON TABLE table TYPE string;

SELECT * FROM table;
CREATE table:2 SET a = "1", b = 2;
