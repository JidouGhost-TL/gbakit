//! Clean-room registry of verified Game Boy Advance save-format definitions.

#![deny(unsafe_code)]

use formatkit_core::Result;
use gba_save::SaveDefinition;

struct EmbeddedEntry {
    id: &'static str,
    game_code: &'static str,
    json: &'static str,
}

const REGISTRY: &[EmbeddedEntry] = &[
    EmbeddedEntry {
        id: "1234",
        game_code: "1234",
        json: include_str!("../data/1234.json"),
    },
    EmbeddedEntry {
        id: "a22j",
        game_code: "A22J",
        json: include_str!("../data/a22j.json"),
    },
    EmbeddedEntry {
        id: "a23j",
        game_code: "A23J",
        json: include_str!("../data/a23j.json"),
    },
    EmbeddedEntry {
        id: "a24j",
        game_code: "A24J",
        json: include_str!("../data/a24j.json"),
    },
    EmbeddedEntry {
        id: "a25j",
        game_code: "A25J",
        json: include_str!("../data/a25j.json"),
    },
    EmbeddedEntry {
        id: "a26j",
        game_code: "A26J",
        json: include_str!("../data/a26j.json"),
    },
    EmbeddedEntry {
        id: "a27j",
        game_code: "A27J",
        json: include_str!("../data/a27j.json"),
    },
    EmbeddedEntry {
        id: "a29j",
        game_code: "A29J",
        json: include_str!("../data/a29j.json"),
    },
    EmbeddedEntry {
        id: "a2ae",
        game_code: "A2AE",
        json: include_str!("../data/a2ae.json"),
    },
    EmbeddedEntry {
        id: "a2cp",
        game_code: "A2CP",
        json: include_str!("../data/a2cp.json"),
    },
    EmbeddedEntry {
        id: "a2de",
        game_code: "A2DE",
        json: include_str!("../data/a2de.json"),
    },
    EmbeddedEntry {
        id: "a2fe",
        game_code: "A2FE",
        json: include_str!("../data/a2fe.json"),
    },
    EmbeddedEntry {
        id: "a2ge",
        game_code: "A2GE",
        json: include_str!("../data/a2ge.json"),
    },
    EmbeddedEntry {
        id: "a2hj",
        game_code: "A2HJ",
        json: include_str!("../data/a2hj.json"),
    },
    EmbeddedEntry {
        id: "a2ij",
        game_code: "A2IJ",
        json: include_str!("../data/a2ij.json"),
    },
    EmbeddedEntry {
        id: "a2jj",
        game_code: "A2JJ",
        json: include_str!("../data/a2jj.json"),
    },
    EmbeddedEntry {
        id: "a2ke",
        game_code: "A2KE",
        json: include_str!("../data/a2ke.json"),
    },
    EmbeddedEntry {
        id: "a2le",
        game_code: "A2LE",
        json: include_str!("../data/a2le.json"),
    },
    EmbeddedEntry {
        id: "a2me",
        game_code: "A2ME",
        json: include_str!("../data/a2me.json"),
    },
    EmbeddedEntry {
        id: "a2ne",
        game_code: "A2NE",
        json: include_str!("../data/a2ne.json"),
    },
    EmbeddedEntry {
        id: "a2oj",
        game_code: "A2OJ",
        json: include_str!("../data/a2oj.json"),
    },
    EmbeddedEntry {
        id: "a2pj",
        game_code: "A2PJ",
        json: include_str!("../data/a2pj.json"),
    },
    EmbeddedEntry {
        id: "a2qe",
        game_code: "A2QE",
        json: include_str!("../data/a2qe.json"),
    },
    EmbeddedEntry {
        id: "a2re",
        game_code: "A2RE",
        json: include_str!("../data/a2re.json"),
    },
    EmbeddedEntry {
        id: "a2se",
        game_code: "A2SE",
        json: include_str!("../data/a2se.json"),
    },
    EmbeddedEntry {
        id: "a2te",
        game_code: "A2TE",
        json: include_str!("../data/a2te.json"),
    },
    EmbeddedEntry {
        id: "a2uj",
        game_code: "A2UJ",
        json: include_str!("../data/a2uj.json"),
    },
    EmbeddedEntry {
        id: "a2vj",
        game_code: "A2VJ",
        json: include_str!("../data/a2vj.json"),
    },
    EmbeddedEntry {
        id: "a2zj",
        game_code: "A2ZJ",
        json: include_str!("../data/a2zj.json"),
    },
    EmbeddedEntry {
        id: "a39j",
        game_code: "A39J",
        json: include_str!("../data/a39j.json"),
    },
    EmbeddedEntry {
        id: "a3ae",
        game_code: "A3AE",
        json: include_str!("../data/a3ae.json"),
    },
    EmbeddedEntry {
        id: "a3ap",
        game_code: "A3AP",
        json: include_str!("../data/a3ap.json"),
    },
    EmbeddedEntry {
        id: "a3ce",
        game_code: "A3CE",
        json: include_str!("../data/a3ce.json"),
    },
    EmbeddedEntry {
        id: "a3de",
        game_code: "A3DE",
        json: include_str!("../data/a3de.json"),
    },
    EmbeddedEntry {
        id: "a3ej",
        game_code: "A3EJ",
        json: include_str!("../data/a3ej.json"),
    },
    EmbeddedEntry {
        id: "a3gj",
        game_code: "A3GJ",
        json: include_str!("../data/a3gj.json"),
    },
    EmbeddedEntry {
        id: "a3ij",
        game_code: "A3IJ",
        json: include_str!("../data/a3ij.json"),
    },
    EmbeddedEntry {
        id: "a3jj",
        game_code: "A3JJ",
        json: include_str!("../data/a3jj.json"),
    },
    EmbeddedEntry {
        id: "a3mp",
        game_code: "A3MP",
        json: include_str!("../data/a3mp.json"),
    },
    EmbeddedEntry {
        id: "a3nj",
        game_code: "A3NJ",
        json: include_str!("../data/a3nj.json"),
    },
    EmbeddedEntry {
        id: "a3oj",
        game_code: "A3OJ",
        json: include_str!("../data/a3oj.json"),
    },
    EmbeddedEntry {
        id: "a3uj",
        game_code: "A3UJ",
        json: include_str!("../data/a3uj.json"),
    },
    EmbeddedEntry {
        id: "a3ve",
        game_code: "A3VE",
        json: include_str!("../data/a3ve.json"),
    },
    EmbeddedEntry {
        id: "a3vp",
        game_code: "A3VP",
        json: include_str!("../data/a3vp.json"),
    },
    EmbeddedEntry {
        id: "a3wj",
        game_code: "A3WJ",
        json: include_str!("../data/a3wj.json"),
    },
    EmbeddedEntry {
        id: "a3xe",
        game_code: "A3XE",
        json: include_str!("../data/a3xe.json"),
    },
    EmbeddedEntry {
        id: "a3ze",
        game_code: "A3ZE",
        json: include_str!("../data/a3ze.json"),
    },
    EmbeddedEntry {
        id: "a4ce",
        game_code: "A4CE",
        json: include_str!("../data/a4ce.json"),
    },
    EmbeddedEntry {
        id: "a4de",
        game_code: "A4DE",
        json: include_str!("../data/a4de.json"),
    },
    EmbeddedEntry {
        id: "a4fe",
        game_code: "A4FE",
        json: include_str!("../data/a4fe.json"),
    },
    EmbeddedEntry {
        id: "a4ge",
        game_code: "A4GE",
        json: include_str!("../data/a4ge.json"),
    },
    EmbeddedEntry {
        id: "a4kj",
        game_code: "A4KJ",
        json: include_str!("../data/a4kj.json"),
    },
    EmbeddedEntry {
        id: "a4lj",
        game_code: "A4LJ",
        json: include_str!("../data/a4lj.json"),
    },
    EmbeddedEntry {
        id: "a4ne",
        game_code: "A4NE",
        json: include_str!("../data/a4ne.json"),
    },
    EmbeddedEntry {
        id: "a4oh",
        game_code: "A4OH",
        json: include_str!("../data/a4oh.json"),
    },
    EmbeddedEntry {
        id: "a4pj",
        game_code: "A4PJ",
        json: include_str!("../data/a4pj.json"),
    },
    EmbeddedEntry {
        id: "a4re",
        game_code: "A4RE",
        json: include_str!("../data/a4re.json"),
    },
    EmbeddedEntry {
        id: "a4sj",
        game_code: "A4SJ",
        json: include_str!("../data/a4sj.json"),
    },
    EmbeddedEntry {
        id: "a4vj",
        game_code: "A4VJ",
        json: include_str!("../data/a4vj.json"),
    },
    EmbeddedEntry {
        id: "a4wj",
        game_code: "A4WJ",
        json: include_str!("../data/a4wj.json"),
    },
    EmbeddedEntry {
        id: "a4xj",
        game_code: "A4XJ",
        json: include_str!("../data/a4xj.json"),
    },
    EmbeddedEntry {
        id: "a52j",
        game_code: "A52J",
        json: include_str!("../data/a52j.json"),
    },
    EmbeddedEntry {
        id: "a53j",
        game_code: "A53J",
        json: include_str!("../data/a53j.json"),
    },
    EmbeddedEntry {
        id: "a54j",
        game_code: "A54J",
        json: include_str!("../data/a54j.json"),
    },
    EmbeddedEntry {
        id: "a56j",
        game_code: "A56J",
        json: include_str!("../data/a56j.json"),
    },
    EmbeddedEntry {
        id: "a59j",
        game_code: "A59J",
        json: include_str!("../data/a59j.json"),
    },
    EmbeddedEntry {
        id: "a5ae",
        game_code: "A5AE",
        json: include_str!("../data/a5ae.json"),
    },
    EmbeddedEntry {
        id: "a5bj",
        game_code: "A5BJ",
        json: include_str!("../data/a5bj.json"),
    },
    EmbeddedEntry {
        id: "a5ce",
        game_code: "A5CE",
        json: include_str!("../data/a5ce.json"),
    },
    EmbeddedEntry {
        id: "a5de",
        game_code: "A5DE",
        json: include_str!("../data/a5de.json"),
    },
    EmbeddedEntry {
        id: "a5ne",
        game_code: "A5NE",
        json: include_str!("../data/a5ne.json"),
    },
    EmbeddedEntry {
        id: "a5pj",
        game_code: "A5PJ",
        json: include_str!("../data/a5pj.json"),
    },
    EmbeddedEntry {
        id: "a5sj",
        game_code: "A5SJ",
        json: include_str!("../data/a5sj.json"),
    },
    EmbeddedEntry {
        id: "a5tj",
        game_code: "A5TJ",
        json: include_str!("../data/a5tj.json"),
    },
    EmbeddedEntry {
        id: "a62e",
        game_code: "A62E",
        json: include_str!("../data/a62e.json"),
    },
    EmbeddedEntry {
        id: "a63j",
        game_code: "A63J",
        json: include_str!("../data/a63j.json"),
    },
    EmbeddedEntry {
        id: "a64j",
        game_code: "A64J",
        json: include_str!("../data/a64j.json"),
    },
    EmbeddedEntry {
        id: "a6be",
        game_code: "A6BE",
        json: include_str!("../data/a6be.json"),
    },
    EmbeddedEntry {
        id: "a6cj",
        game_code: "A6CJ",
        json: include_str!("../data/a6cj.json"),
    },
    EmbeddedEntry {
        id: "a6de",
        game_code: "A6DE",
        json: include_str!("../data/a6de.json"),
    },
    EmbeddedEntry {
        id: "a6gj",
        game_code: "A6GJ",
        json: include_str!("../data/a6gj.json"),
    },
    EmbeddedEntry {
        id: "a6me",
        game_code: "A6ME",
        json: include_str!("../data/a6me.json"),
    },
    EmbeddedEntry {
        id: "a6oe",
        game_code: "A6OE",
        json: include_str!("../data/a6oe.json"),
    },
    EmbeddedEntry {
        id: "a6re",
        game_code: "A6RE",
        json: include_str!("../data/a6re.json"),
    },
    EmbeddedEntry {
        id: "a6sj",
        game_code: "A6SJ",
        json: include_str!("../data/a6sj.json"),
    },
    EmbeddedEntry {
        id: "a73j",
        game_code: "A73J",
        json: include_str!("../data/a73j.json"),
    },
    EmbeddedEntry {
        id: "a7ae",
        game_code: "A7AE",
        json: include_str!("../data/a7ae.json"),
    },
    EmbeddedEntry {
        id: "a7gj",
        game_code: "A7GJ",
        json: include_str!("../data/a7gj.json"),
    },
    EmbeddedEntry {
        id: "a7he",
        game_code: "A7HE",
        json: include_str!("../data/a7he.json"),
    },
    EmbeddedEntry {
        id: "a7ij",
        game_code: "A7IJ",
        json: include_str!("../data/a7ij.json"),
    },
    EmbeddedEntry {
        id: "a7ke",
        game_code: "A7KE",
        json: include_str!("../data/a7ke.json"),
    },
    EmbeddedEntry {
        id: "a7kj",
        game_code: "A7KJ",
        json: include_str!("../data/a7kj.json"),
    },
    EmbeddedEntry {
        id: "a7oe",
        game_code: "A7OE",
        json: include_str!("../data/a7oe.json"),
    },
    EmbeddedEntry {
        id: "a7te",
        game_code: "A7TE",
        json: include_str!("../data/a7te.json"),
    },
    EmbeddedEntry {
        id: "a82j",
        game_code: "A82J",
        json: include_str!("../data/a82j.json"),
    },
    EmbeddedEntry {
        id: "a84e",
        game_code: "A84E",
        json: include_str!("../data/a84e.json"),
    },
    EmbeddedEntry {
        id: "a86j",
        game_code: "A86J",
        json: include_str!("../data/a86j.json"),
    },
    EmbeddedEntry {
        id: "a87j",
        game_code: "A87J",
        json: include_str!("../data/a87j.json"),
    },
    EmbeddedEntry {
        id: "a88e",
        game_code: "A88E",
        json: include_str!("../data/a88e.json"),
    },
    EmbeddedEntry {
        id: "a89e",
        game_code: "A89E",
        json: include_str!("../data/a89e.json"),
    },
    EmbeddedEntry {
        id: "a8dj",
        game_code: "A8DJ",
        json: include_str!("../data/a8dj.json"),
    },
    EmbeddedEntry {
        id: "a8ej",
        game_code: "A8EJ",
        json: include_str!("../data/a8ej.json"),
    },
    EmbeddedEntry {
        id: "a8gj",
        game_code: "A8GJ",
        json: include_str!("../data/a8gj.json"),
    },
    EmbeddedEntry {
        id: "a8lj",
        game_code: "A8LJ",
        json: include_str!("../data/a8lj.json"),
    },
    EmbeddedEntry {
        id: "a8mj",
        game_code: "A8MJ",
        json: include_str!("../data/a8mj.json"),
    },
    EmbeddedEntry {
        id: "a8nj",
        game_code: "A8NJ",
        json: include_str!("../data/a8nj.json"),
    },
    EmbeddedEntry {
        id: "a8oj",
        game_code: "A8OJ",
        json: include_str!("../data/a8oj.json"),
    },
    EmbeddedEntry {
        id: "a8pj",
        game_code: "A8PJ",
        json: include_str!("../data/a8pj.json"),
    },
    EmbeddedEntry {
        id: "a8rj",
        game_code: "A8RJ",
        json: include_str!("../data/a8rj.json"),
    },
    EmbeddedEntry {
        id: "a8sp",
        game_code: "A8SP",
        json: include_str!("../data/a8sp.json"),
    },
    EmbeddedEntry {
        id: "a8tj",
        game_code: "A8TJ",
        json: include_str!("../data/a8tj.json"),
    },
    EmbeddedEntry {
        id: "a8vj",
        game_code: "A8VJ",
        json: include_str!("../data/a8vj.json"),
    },
    EmbeddedEntry {
        id: "a8yj",
        game_code: "A8YJ",
        json: include_str!("../data/a8yj.json"),
    },
    EmbeddedEntry {
        id: "a8zj",
        game_code: "A8ZJ",
        json: include_str!("../data/a8zj.json"),
    },
    EmbeddedEntry {
        id: "a9de",
        game_code: "A9DE",
        json: include_str!("../data/a9de.json"),
    },
    EmbeddedEntry {
        id: "a9hj",
        game_code: "A9HJ",
        json: include_str!("../data/a9hj.json"),
    },
    EmbeddedEntry {
        id: "a9kj",
        game_code: "A9KJ",
        json: include_str!("../data/a9kj.json"),
    },
    EmbeddedEntry {
        id: "a9lj",
        game_code: "A9LJ",
        json: include_str!("../data/a9lj.json"),
    },
    EmbeddedEntry {
        id: "a9me",
        game_code: "A9ME",
        json: include_str!("../data/a9me.json"),
    },
    EmbeddedEntry {
        id: "a9pj",
        game_code: "A9PJ",
        json: include_str!("../data/a9pj.json"),
    },
    EmbeddedEntry {
        id: "a9qj",
        game_code: "A9QJ",
        json: include_str!("../data/a9qj.json"),
    },
    EmbeddedEntry {
        id: "a9sj",
        game_code: "A9SJ",
        json: include_str!("../data/a9sj.json"),
    },
    EmbeddedEntry {
        id: "a9tj",
        game_code: "A9TJ",
        json: include_str!("../data/a9tj.json"),
    },
    EmbeddedEntry {
        id: "aa2e",
        game_code: "AA2E",
        json: include_str!("../data/aa2e.json"),
    },
    EmbeddedEntry {
        id: "aa3e",
        game_code: "AA3E",
        json: include_str!("../data/aa3e.json"),
    },
    EmbeddedEntry {
        id: "aa4j",
        game_code: "AA4J",
        json: include_str!("../data/aa4j.json"),
    },
    EmbeddedEntry {
        id: "aa5j",
        game_code: "AA5J",
        json: include_str!("../data/aa5j.json"),
    },
    EmbeddedEntry {
        id: "aa6e",
        game_code: "AA6E",
        json: include_str!("../data/aa6e.json"),
    },
    EmbeddedEntry {
        id: "aa7e",
        game_code: "AA7E",
        json: include_str!("../data/aa7e.json"),
    },
    EmbeddedEntry {
        id: "aa9e",
        game_code: "AA9E",
        json: include_str!("../data/aa9e.json"),
    },
    EmbeddedEntry {
        id: "aagj",
        game_code: "AAGJ",
        json: include_str!("../data/aagj.json"),
    },
    EmbeddedEntry {
        id: "aaij",
        game_code: "AAIJ",
        json: include_str!("../data/aaij.json"),
    },
    EmbeddedEntry {
        id: "aajj",
        game_code: "AAJJ",
        json: include_str!("../data/aajj.json"),
    },
    EmbeddedEntry {
        id: "aake",
        game_code: "AAKE",
        json: include_str!("../data/aake.json"),
    },
    EmbeddedEntry {
        id: "aalj",
        game_code: "AALJ",
        json: include_str!("../data/aalj.json"),
    },
    EmbeddedEntry {
        id: "aame",
        game_code: "AAME",
        json: include_str!("../data/aame.json"),
    },
    EmbeddedEntry {
        id: "aanj",
        game_code: "AANJ",
        json: include_str!("../data/aanj.json"),
    },
    EmbeddedEntry {
        id: "aaoe",
        game_code: "AAOE",
        json: include_str!("../data/aaoe.json"),
    },
    EmbeddedEntry {
        id: "aapj",
        game_code: "AAPJ",
        json: include_str!("../data/aapj.json"),
    },
    EmbeddedEntry {
        id: "aare",
        game_code: "AARE",
        json: include_str!("../data/aare.json"),
    },
    EmbeddedEntry {
        id: "aasj",
        game_code: "AASJ",
        json: include_str!("../data/aasj.json"),
    },
    EmbeddedEntry {
        id: "aatj",
        game_code: "AATJ",
        json: include_str!("../data/aatj.json"),
    },
    EmbeddedEntry {
        id: "aauj",
        game_code: "AAUJ",
        json: include_str!("../data/aauj.json"),
    },
    EmbeddedEntry {
        id: "aaxj",
        game_code: "AAXJ",
        json: include_str!("../data/aaxj.json"),
    },
    EmbeddedEntry {
        id: "aazj",
        game_code: "AAZJ",
        json: include_str!("../data/aazj.json"),
    },
    EmbeddedEntry {
        id: "ab2e",
        game_code: "AB2E",
        json: include_str!("../data/ab2e.json"),
    },
    EmbeddedEntry {
        id: "ab4e",
        game_code: "AB4E",
        json: include_str!("../data/ab4e.json"),
    },
    EmbeddedEntry {
        id: "ab6p",
        game_code: "AB6P",
        json: include_str!("../data/ab6p.json"),
    },
    EmbeddedEntry {
        id: "ab7j",
        game_code: "AB7J",
        json: include_str!("../data/ab7j.json"),
    },
    EmbeddedEntry {
        id: "ab8j",
        game_code: "AB8J",
        json: include_str!("../data/ab8j.json"),
    },
    EmbeddedEntry {
        id: "ab9e",
        game_code: "AB9E",
        json: include_str!("../data/ab9e.json"),
    },
    EmbeddedEntry {
        id: "abcj",
        game_code: "ABCJ",
        json: include_str!("../data/abcj.json"),
    },
    EmbeddedEntry {
        id: "abdp",
        game_code: "ABDP",
        json: include_str!("../data/abdp.json"),
    },
    EmbeddedEntry {
        id: "abee",
        game_code: "ABEE",
        json: include_str!("../data/abee.json"),
    },
    EmbeddedEntry {
        id: "abfe",
        game_code: "ABFE",
        json: include_str!("../data/abfe.json"),
    },
    EmbeddedEntry {
        id: "abgj",
        game_code: "ABGJ",
        json: include_str!("../data/abgj.json"),
    },
    EmbeddedEntry {
        id: "abij",
        game_code: "ABIJ",
        json: include_str!("../data/abij.json"),
    },
    EmbeddedEntry {
        id: "abje",
        game_code: "ABJE",
        json: include_str!("../data/abje.json"),
    },
    EmbeddedEntry {
        id: "abke",
        game_code: "ABKE",
        json: include_str!("../data/abke.json"),
    },
    EmbeddedEntry {
        id: "abqe",
        game_code: "ABQE",
        json: include_str!("../data/abqe.json"),
    },
    EmbeddedEntry {
        id: "abre",
        game_code: "ABRE",
        json: include_str!("../data/abre.json"),
    },
    EmbeddedEntry {
        id: "abse",
        game_code: "ABSE",
        json: include_str!("../data/abse.json"),
    },
    EmbeddedEntry {
        id: "abue",
        game_code: "ABUE",
        json: include_str!("../data/abue.json"),
    },
    EmbeddedEntry {
        id: "ac2j",
        game_code: "AC2J",
        json: include_str!("../data/ac2j.json"),
    },
    EmbeddedEntry {
        id: "ac4j",
        game_code: "AC4J",
        json: include_str!("../data/ac4j.json"),
    },
    EmbeddedEntry {
        id: "ac5e",
        game_code: "AC5E",
        json: include_str!("../data/ac5e.json"),
    },
    EmbeddedEntry {
        id: "ac8e",
        game_code: "AC8E",
        json: include_str!("../data/ac8e.json"),
    },
    EmbeddedEntry {
        id: "acbe",
        game_code: "ACBE",
        json: include_str!("../data/acbe.json"),
    },
    EmbeddedEntry {
        id: "acce",
        game_code: "ACCE",
        json: include_str!("../data/acce.json"),
    },
    EmbeddedEntry {
        id: "acge",
        game_code: "ACGE",
        json: include_str!("../data/acge.json"),
    },
    EmbeddedEntry {
        id: "achj",
        game_code: "ACHJ",
        json: include_str!("../data/achj.json"),
    },
    EmbeddedEntry {
        id: "acij",
        game_code: "ACIJ",
        json: include_str!("../data/acij.json"),
    },
    EmbeddedEntry {
        id: "acjj",
        game_code: "ACJJ",
        json: include_str!("../data/acjj.json"),
    },
    EmbeddedEntry {
        id: "acke",
        game_code: "ACKE",
        json: include_str!("../data/acke.json"),
    },
    EmbeddedEntry {
        id: "aclj",
        game_code: "ACLJ",
        json: include_str!("../data/aclj.json"),
    },
    EmbeddedEntry {
        id: "acme",
        game_code: "ACME",
        json: include_str!("../data/acme.json"),
    },
    EmbeddedEntry {
        id: "acoj",
        game_code: "ACOJ",
        json: include_str!("../data/acoj.json"),
    },
    EmbeddedEntry {
        id: "acre",
        game_code: "ACRE",
        json: include_str!("../data/acre.json"),
    },
    EmbeddedEntry {
        id: "actx",
        game_code: "ACTX",
        json: include_str!("../data/actx.json"),
    },
    EmbeddedEntry {
        id: "acve",
        game_code: "ACVE",
        json: include_str!("../data/acve.json"),
    },
    EmbeddedEntry {
        id: "acxe",
        game_code: "ACXE",
        json: include_str!("../data/acxe.json"),
    },
    EmbeddedEntry {
        id: "acye",
        game_code: "ACYE",
        json: include_str!("../data/acye.json"),
    },
    EmbeddedEntry {
        id: "ad2p",
        game_code: "AD2P",
        json: include_str!("../data/ad2p.json"),
    },
    EmbeddedEntry {
        id: "ad3e",
        game_code: "AD3E",
        json: include_str!("../data/ad3e.json"),
    },
    EmbeddedEntry {
        id: "ad4e",
        game_code: "AD4E",
        json: include_str!("../data/ad4e.json"),
    },
    EmbeddedEntry {
        id: "ad5j",
        game_code: "AD5J",
        json: include_str!("../data/ad5j.json"),
    },
    EmbeddedEntry {
        id: "ad6e",
        game_code: "AD6E",
        json: include_str!("../data/ad6e.json"),
    },
    EmbeddedEntry {
        id: "ad9e",
        game_code: "AD9E",
        json: include_str!("../data/ad9e.json"),
    },
    EmbeddedEntry {
        id: "adbe",
        game_code: "ADBE",
        json: include_str!("../data/adbe.json"),
    },
    EmbeddedEntry {
        id: "addj",
        game_code: "ADDJ",
        json: include_str!("../data/addj.json"),
    },
    EmbeddedEntry {
        id: "adej",
        game_code: "ADEJ",
        json: include_str!("../data/adej.json"),
    },
    EmbeddedEntry {
        id: "adfe",
        game_code: "ADFE",
        json: include_str!("../data/adfe.json"),
    },
    EmbeddedEntry {
        id: "adhe",
        game_code: "ADHE",
        json: include_str!("../data/adhe.json"),
    },
    EmbeddedEntry {
        id: "adne",
        game_code: "ADNE",
        json: include_str!("../data/adne.json"),
    },
    EmbeddedEntry {
        id: "adpj",
        game_code: "ADPJ",
        json: include_str!("../data/adpj.json"),
    },
    EmbeddedEntry {
        id: "adqe",
        game_code: "ADQE",
        json: include_str!("../data/adqe.json"),
    },
    EmbeddedEntry {
        id: "adsj",
        game_code: "ADSJ",
        json: include_str!("../data/adsj.json"),
    },
    EmbeddedEntry {
        id: "adue",
        game_code: "ADUE",
        json: include_str!("../data/adue.json"),
    },
    EmbeddedEntry {
        id: "adve",
        game_code: "ADVE",
        json: include_str!("../data/adve.json"),
    },
    EmbeddedEntry {
        id: "adyj",
        game_code: "ADYJ",
        json: include_str!("../data/adyj.json"),
    },
    EmbeddedEntry {
        id: "adze",
        game_code: "ADZE",
        json: include_str!("../data/adze.json"),
    },
    EmbeddedEntry {
        id: "ae2e",
        game_code: "AE2E",
        json: include_str!("../data/ae2e.json"),
    },
    EmbeddedEntry {
        id: "ae3e",
        game_code: "AE3E",
        json: include_str!("../data/ae3e.json"),
    },
    EmbeddedEntry {
        id: "ae7e",
        game_code: "AE7E",
        json: include_str!("../data/ae7e.json"),
    },
    EmbeddedEntry {
        id: "aeaj",
        game_code: "AEAJ",
        json: include_str!("../data/aeaj.json"),
    },
    EmbeddedEntry {
        id: "aedp",
        game_code: "AEDP",
        json: include_str!("../data/aedp.json"),
    },
    EmbeddedEntry {
        id: "aekj",
        game_code: "AEKJ",
        json: include_str!("../data/aekj.json"),
    },
    EmbeddedEntry {
        id: "aepp",
        game_code: "AEPP",
        json: include_str!("../data/aepp.json"),
    },
    EmbeddedEntry {
        id: "aexe",
        game_code: "AEXE",
        json: include_str!("../data/aexe.json"),
    },
    EmbeddedEntry {
        id: "af3j",
        game_code: "AF3J",
        json: include_str!("../data/af3j.json"),
    },
    EmbeddedEntry {
        id: "af4j",
        game_code: "AF4J",
        json: include_str!("../data/af4j.json"),
    },
    EmbeddedEntry {
        id: "af5e",
        game_code: "AF5E",
        json: include_str!("../data/af5e.json"),
    },
    EmbeddedEntry {
        id: "af7j",
        game_code: "AF7J",
        json: include_str!("../data/af7j.json"),
    },
    EmbeddedEntry {
        id: "af8e",
        game_code: "AF8E",
        json: include_str!("../data/af8e.json"),
    },
    EmbeddedEntry {
        id: "af9j",
        game_code: "AF9J",
        json: include_str!("../data/af9j.json"),
    },
    EmbeddedEntry {
        id: "afaj",
        game_code: "AFAJ",
        json: include_str!("../data/afaj.json"),
    },
    EmbeddedEntry {
        id: "afbj",
        game_code: "AFBJ",
        json: include_str!("../data/afbj.json"),
    },
    EmbeddedEntry {
        id: "afcj",
        game_code: "AFCJ",
        json: include_str!("../data/afcj.json"),
    },
    EmbeddedEntry {
        id: "afej",
        game_code: "AFEJ",
        json: include_str!("../data/afej.json"),
    },
    EmbeddedEntry {
        id: "affe",
        game_code: "AFFE",
        json: include_str!("../data/affe.json"),
    },
    EmbeddedEntry {
        id: "afje",
        game_code: "AFJE",
        json: include_str!("../data/afje.json"),
    },
    EmbeddedEntry {
        id: "aflp",
        game_code: "AFLP",
        json: include_str!("../data/aflp.json"),
    },
    EmbeddedEntry {
        id: "afmj",
        game_code: "AFMJ",
        json: include_str!("../data/afmj.json"),
    },
    EmbeddedEntry {
        id: "afnj",
        game_code: "AFNJ",
        json: include_str!("../data/afnj.json"),
    },
    EmbeddedEntry {
        id: "afpe",
        game_code: "AFPE",
        json: include_str!("../data/afpe.json"),
    },
    EmbeddedEntry {
        id: "afre",
        game_code: "AFRE",
        json: include_str!("../data/afre.json"),
    },
    EmbeddedEntry {
        id: "afuj",
        game_code: "AFUJ",
        json: include_str!("../data/afuj.json"),
    },
    EmbeddedEntry {
        id: "afwj",
        game_code: "AFWJ",
        json: include_str!("../data/afwj.json"),
    },
    EmbeddedEntry {
        id: "afxe",
        game_code: "AFXE",
        json: include_str!("../data/afxe.json"),
    },
    EmbeddedEntry {
        id: "afye",
        game_code: "AFYE",
        json: include_str!("../data/afye.json"),
    },
    EmbeddedEntry {
        id: "afze",
        game_code: "AFZE",
        json: include_str!("../data/afze.json"),
    },
    EmbeddedEntry {
        id: "ag2j",
        game_code: "AG2J",
        json: include_str!("../data/ag2j.json"),
    },
    EmbeddedEntry {
        id: "ag5e",
        game_code: "AG5E",
        json: include_str!("../data/ag5e.json"),
    },
    EmbeddedEntry {
        id: "ag6j",
        game_code: "AG6J",
        json: include_str!("../data/ag6j.json"),
    },
    EmbeddedEntry {
        id: "ag7j",
        game_code: "AG7J",
        json: include_str!("../data/ag7j.json"),
    },
    EmbeddedEntry {
        id: "ag8e",
        game_code: "AG8E",
        json: include_str!("../data/ag8e.json"),
    },
    EmbeddedEntry {
        id: "agae",
        game_code: "AGAE",
        json: include_str!("../data/agae.json"),
    },
    EmbeddedEntry {
        id: "agbj",
        game_code: "AGBJ",
        json: include_str!("../data/agbj.json"),
    },
    EmbeddedEntry {
        id: "agce",
        game_code: "AGCE",
        json: include_str!("../data/agce.json"),
    },
    EmbeddedEntry {
        id: "agde",
        game_code: "AGDE",
        json: include_str!("../data/agde.json"),
    },
    EmbeddedEntry {
        id: "agfe",
        game_code: "AGFE",
        json: include_str!("../data/agfe.json"),
    },
    EmbeddedEntry {
        id: "aghj",
        game_code: "AGHJ",
        json: include_str!("../data/aghj.json"),
    },
    EmbeddedEntry {
        id: "agij",
        game_code: "AGIJ",
        json: include_str!("../data/agij.json"),
    },
    EmbeddedEntry {
        id: "agke",
        game_code: "AGKE",
        json: include_str!("../data/agke.json"),
    },
    EmbeddedEntry {
        id: "aglc",
        game_code: "AGLC",
        json: include_str!("../data/aglc.json"),
    },
    EmbeddedEntry {
        id: "agmj",
        game_code: "AGMJ",
        json: include_str!("../data/agmj.json"),
    },
    EmbeddedEntry {
        id: "agnj",
        game_code: "AGNJ",
        json: include_str!("../data/agnj.json"),
    },
    EmbeddedEntry {
        id: "agoj",
        game_code: "AGOJ",
        json: include_str!("../data/agoj.json"),
    },
    EmbeddedEntry {
        id: "agse",
        game_code: "AGSE",
        json: include_str!("../data/agse.json"),
    },
    EmbeddedEntry {
        id: "agtj",
        game_code: "AGTJ",
        json: include_str!("../data/agtj.json"),
    },
    EmbeddedEntry {
        id: "agvj",
        game_code: "AGVJ",
        json: include_str!("../data/agvj.json"),
    },
    EmbeddedEntry {
        id: "agxe",
        game_code: "AGXE",
        json: include_str!("../data/agxe.json"),
    },
    EmbeddedEntry {
        id: "agzj",
        game_code: "AGZJ",
        json: include_str!("../data/agzj.json"),
    },
    EmbeddedEntry {
        id: "ah2e",
        game_code: "AH2E",
        json: include_str!("../data/ah2e.json"),
    },
    EmbeddedEntry {
        id: "ah3p",
        game_code: "AH3P",
        json: include_str!("../data/ah3p.json"),
    },
    EmbeddedEntry {
        id: "ah4p",
        game_code: "AH4P",
        json: include_str!("../data/ah4p.json"),
    },
    EmbeddedEntry {
        id: "ah5j",
        game_code: "AH5J",
        json: include_str!("../data/ah5j.json"),
    },
    EmbeddedEntry {
        id: "ah6e",
        game_code: "AH6E",
        json: include_str!("../data/ah6e.json"),
    },
    EmbeddedEntry {
        id: "ah7j",
        game_code: "AH7J",
        json: include_str!("../data/ah7j.json"),
    },
    EmbeddedEntry {
        id: "ahaj",
        game_code: "AHAJ",
        json: include_str!("../data/ahaj.json"),
    },
    EmbeddedEntry {
        id: "ahbj",
        game_code: "AHBJ",
        json: include_str!("../data/ahbj.json"),
    },
    EmbeddedEntry {
        id: "ahcj",
        game_code: "AHCJ",
        json: include_str!("../data/ahcj.json"),
    },
    EmbeddedEntry {
        id: "ahej",
        game_code: "AHEJ",
        json: include_str!("../data/ahej.json"),
    },
    EmbeddedEntry {
        id: "ahhe",
        game_code: "AHHE",
        json: include_str!("../data/ahhe.json"),
    },
    EmbeddedEntry {
        id: "ahij",
        game_code: "AHIJ",
        json: include_str!("../data/ahij.json"),
    },
    EmbeddedEntry {
        id: "ahje",
        game_code: "AHJE",
        json: include_str!("../data/ahje.json"),
    },
    EmbeddedEntry {
        id: "ahkj",
        game_code: "AHKJ",
        json: include_str!("../data/ahkj.json"),
    },
    EmbeddedEntry {
        id: "ahle",
        game_code: "AHLE",
        json: include_str!("../data/ahle.json"),
    },
    EmbeddedEntry {
        id: "ahmj",
        game_code: "AHMJ",
        json: include_str!("../data/ahmj.json"),
    },
    EmbeddedEntry {
        id: "ahne",
        game_code: "AHNE",
        json: include_str!("../data/ahne.json"),
    },
    EmbeddedEntry {
        id: "ahoe",
        game_code: "AHOE",
        json: include_str!("../data/ahoe.json"),
    },
    EmbeddedEntry {
        id: "ahqj",
        game_code: "AHQJ",
        json: include_str!("../data/ahqj.json"),
    },
    EmbeddedEntry {
        id: "ahre",
        game_code: "AHRE",
        json: include_str!("../data/ahre.json"),
    },
    EmbeddedEntry {
        id: "ahsj",
        game_code: "AHSJ",
        json: include_str!("../data/ahsj.json"),
    },
    EmbeddedEntry {
        id: "ahue",
        game_code: "AHUE",
        json: include_str!("../data/ahue.json"),
    },
    EmbeddedEntry {
        id: "ahvj",
        game_code: "AHVJ",
        json: include_str!("../data/ahvj.json"),
    },
    EmbeddedEntry {
        id: "ahwe",
        game_code: "AHWE",
        json: include_str!("../data/ahwe.json"),
    },
    EmbeddedEntry {
        id: "ahxj",
        game_code: "AHXJ",
        json: include_str!("../data/ahxj.json"),
    },
    EmbeddedEntry {
        id: "ai7j",
        game_code: "AI7J",
        json: include_str!("../data/ai7j.json"),
    },
    EmbeddedEntry {
        id: "ai9j",
        game_code: "AI9J",
        json: include_str!("../data/ai9j.json"),
    },
    EmbeddedEntry {
        id: "aibj",
        game_code: "AIBJ",
        json: include_str!("../data/aibj.json"),
    },
    EmbeddedEntry {
        id: "aicj",
        game_code: "AICJ",
        json: include_str!("../data/aicj.json"),
    },
    EmbeddedEntry {
        id: "aide",
        game_code: "AIDE",
        json: include_str!("../data/aide.json"),
    },
    EmbeddedEntry {
        id: "aiej",
        game_code: "AIEJ",
        json: include_str!("../data/aiej.json"),
    },
    EmbeddedEntry {
        id: "ainj",
        game_code: "AINJ",
        json: include_str!("../data/ainj.json"),
    },
    EmbeddedEntry {
        id: "aioe",
        game_code: "AIOE",
        json: include_str!("../data/aioe.json"),
    },
    EmbeddedEntry {
        id: "aipe",
        game_code: "AIPE",
        json: include_str!("../data/aipe.json"),
    },
    EmbeddedEntry {
        id: "aisp",
        game_code: "AISP",
        json: include_str!("../data/aisp.json"),
    },
    EmbeddedEntry {
        id: "aj2j",
        game_code: "AJ2J",
        json: include_str!("../data/aj2j.json"),
    },
    EmbeddedEntry {
        id: "aj3e",
        game_code: "AJ3E",
        json: include_str!("../data/aj3e.json"),
    },
    EmbeddedEntry {
        id: "aj6j",
        game_code: "AJ6J",
        json: include_str!("../data/aj6j.json"),
    },
    EmbeddedEntry {
        id: "aj8j",
        game_code: "AJ8J",
        json: include_str!("../data/aj8j.json"),
    },
    EmbeddedEntry {
        id: "aj9j",
        game_code: "AJ9J",
        json: include_str!("../data/aj9j.json"),
    },
    EmbeddedEntry {
        id: "ajae",
        game_code: "AJAE",
        json: include_str!("../data/ajae.json"),
    },
    EmbeddedEntry {
        id: "ajce",
        game_code: "AJCE",
        json: include_str!("../data/ajce.json"),
    },
    EmbeddedEntry {
        id: "ajej",
        game_code: "AJEJ",
        json: include_str!("../data/ajej.json"),
    },
    EmbeddedEntry {
        id: "ajge",
        game_code: "AJGE",
        json: include_str!("../data/ajge.json"),
    },
    EmbeddedEntry {
        id: "ajhj",
        game_code: "AJHJ",
        json: include_str!("../data/ajhj.json"),
    },
    EmbeddedEntry {
        id: "ajje",
        game_code: "AJJE",
        json: include_str!("../data/ajje.json"),
    },
    EmbeddedEntry {
        id: "ajkj",
        game_code: "AJKJ",
        json: include_str!("../data/ajkj.json"),
    },
    EmbeddedEntry {
        id: "ajle",
        game_code: "AJLE",
        json: include_str!("../data/ajle.json"),
    },
    EmbeddedEntry {
        id: "ajme",
        game_code: "AJME",
        json: include_str!("../data/ajme.json"),
    },
    EmbeddedEntry {
        id: "ajoj",
        game_code: "AJOJ",
        json: include_str!("../data/ajoj.json"),
    },
    EmbeddedEntry {
        id: "ajpj",
        game_code: "AJPJ",
        json: include_str!("../data/ajpj.json"),
    },
    EmbeddedEntry {
        id: "ajqp",
        game_code: "AJQP",
        json: include_str!("../data/ajqp.json"),
    },
    EmbeddedEntry {
        id: "ajre",
        game_code: "AJRE",
        json: include_str!("../data/ajre.json"),
    },
    EmbeddedEntry {
        id: "ajsj",
        game_code: "AJSJ",
        json: include_str!("../data/ajsj.json"),
    },
    EmbeddedEntry {
        id: "ajuj",
        game_code: "AJUJ",
        json: include_str!("../data/ajuj.json"),
    },
    EmbeddedEntry {
        id: "ak2e",
        game_code: "AK2E",
        json: include_str!("../data/ak2e.json"),
    },
    EmbeddedEntry {
        id: "ak3e",
        game_code: "AK3E",
        json: include_str!("../data/ak3e.json"),
    },
    EmbeddedEntry {
        id: "ak4j",
        game_code: "AK4J",
        json: include_str!("../data/ak4j.json"),
    },
    EmbeddedEntry {
        id: "ak7j",
        game_code: "AK7J",
        json: include_str!("../data/ak7j.json"),
    },
    EmbeddedEntry {
        id: "ak8e",
        game_code: "AK8E",
        json: include_str!("../data/ak8e.json"),
    },
    EmbeddedEntry {
        id: "ak9e",
        game_code: "AK9E",
        json: include_str!("../data/ak9e.json"),
    },
    EmbeddedEntry {
        id: "akaj",
        game_code: "AKAJ",
        json: include_str!("../data/akaj.json"),
    },
    EmbeddedEntry {
        id: "akbe",
        game_code: "AKBE",
        json: include_str!("../data/akbe.json"),
    },
    EmbeddedEntry {
        id: "akdj",
        game_code: "AKDJ",
        json: include_str!("../data/akdj.json"),
    },
    EmbeddedEntry {
        id: "akej",
        game_code: "AKEJ",
        json: include_str!("../data/akej.json"),
    },
    EmbeddedEntry {
        id: "akfe",
        game_code: "AKFE",
        json: include_str!("../data/akfe.json"),
    },
    EmbeddedEntry {
        id: "akge",
        game_code: "AKGE",
        json: include_str!("../data/akge.json"),
    },
    EmbeddedEntry {
        id: "akle",
        game_code: "AKLE",
        json: include_str!("../data/akle.json"),
    },
    EmbeddedEntry {
        id: "akoe",
        game_code: "AKOE",
        json: include_str!("../data/akoe.json"),
    },
    EmbeddedEntry {
        id: "akrp",
        game_code: "AKRP",
        json: include_str!("../data/akrp.json"),
    },
    EmbeddedEntry {
        id: "akxe",
        game_code: "AKXE",
        json: include_str!("../data/akxe.json"),
    },
    EmbeddedEntry {
        id: "al2p",
        game_code: "AL2P",
        json: include_str!("../data/al2p.json"),
    },
    EmbeddedEntry {
        id: "al4e",
        game_code: "AL4E",
        json: include_str!("../data/al4e.json"),
    },
    EmbeddedEntry {
        id: "albe",
        game_code: "ALBE",
        json: include_str!("../data/albe.json"),
    },
    EmbeddedEntry {
        id: "alde",
        game_code: "ALDE",
        json: include_str!("../data/alde.json"),
    },
    EmbeddedEntry {
        id: "alep",
        game_code: "ALEP",
        json: include_str!("../data/alep.json"),
    },
    EmbeddedEntry {
        id: "alfp",
        game_code: "ALFP",
        json: include_str!("../data/alfp.json"),
    },
    EmbeddedEntry {
        id: "alge",
        game_code: "ALGE",
        json: include_str!("../data/alge.json"),
    },
    EmbeddedEntry {
        id: "alhj",
        game_code: "ALHJ",
        json: include_str!("../data/alhj.json"),
    },
    EmbeddedEntry {
        id: "almj",
        game_code: "ALMJ",
        json: include_str!("../data/almj.json"),
    },
    EmbeddedEntry {
        id: "alne",
        game_code: "ALNE",
        json: include_str!("../data/alne.json"),
    },
    EmbeddedEntry {
        id: "aloe",
        game_code: "ALOE",
        json: include_str!("../data/aloe.json"),
    },
    EmbeddedEntry {
        id: "alpe",
        game_code: "ALPE",
        json: include_str!("../data/alpe.json"),
    },
    EmbeddedEntry {
        id: "alqj",
        game_code: "ALQJ",
        json: include_str!("../data/alqj.json"),
    },
    EmbeddedEntry {
        id: "alre",
        game_code: "ALRE",
        json: include_str!("../data/alre.json"),
    },
    EmbeddedEntry {
        id: "alse",
        game_code: "ALSE",
        json: include_str!("../data/alse.json"),
    },
    EmbeddedEntry {
        id: "alup",
        game_code: "ALUP",
        json: include_str!("../data/alup.json"),
    },
    EmbeddedEntry {
        id: "alve",
        game_code: "ALVE",
        json: include_str!("../data/alve.json"),
    },
    EmbeddedEntry {
        id: "alxp",
        game_code: "ALXP",
        json: include_str!("../data/alxp.json"),
    },
    EmbeddedEntry {
        id: "am2p",
        game_code: "AM2P",
        json: include_str!("../data/am2p.json"),
    },
    EmbeddedEntry {
        id: "am4j",
        game_code: "AM4J",
        json: include_str!("../data/am4j.json"),
    },
    EmbeddedEntry {
        id: "am6e",
        game_code: "AM6E",
        json: include_str!("../data/am6e.json"),
    },
    EmbeddedEntry {
        id: "am7j",
        game_code: "AM7J",
        json: include_str!("../data/am7j.json"),
    },
    EmbeddedEntry {
        id: "am8p",
        game_code: "AM8P",
        json: include_str!("../data/am8p.json"),
    },
    EmbeddedEntry {
        id: "am9p",
        game_code: "AM9P",
        json: include_str!("../data/am9p.json"),
    },
    EmbeddedEntry {
        id: "amae",
        game_code: "AMAE",
        json: include_str!("../data/amae.json"),
    },
    EmbeddedEntry {
        id: "ambj",
        game_code: "AMBJ",
        json: include_str!("../data/ambj.json"),
    },
    EmbeddedEntry {
        id: "amcj",
        game_code: "AMCJ",
        json: include_str!("../data/amcj.json"),
    },
    EmbeddedEntry {
        id: "amfe",
        game_code: "AMFE",
        json: include_str!("../data/amfe.json"),
    },
    EmbeddedEntry {
        id: "amge",
        game_code: "AMGE",
        json: include_str!("../data/amge.json"),
    },
    EmbeddedEntry {
        id: "amhp",
        game_code: "AMHP",
        json: include_str!("../data/amhp.json"),
    },
    EmbeddedEntry {
        id: "amjj",
        game_code: "AMJJ",
        json: include_str!("../data/amjj.json"),
    },
    EmbeddedEntry {
        id: "amke",
        game_code: "AMKE",
        json: include_str!("../data/amke.json"),
    },
    EmbeddedEntry {
        id: "amle",
        game_code: "AMLE",
        json: include_str!("../data/amle.json"),
    },
    EmbeddedEntry {
        id: "ammj",
        game_code: "AMMJ",
        json: include_str!("../data/ammj.json"),
    },
    EmbeddedEntry {
        id: "amnj",
        game_code: "AMNJ",
        json: include_str!("../data/amnj.json"),
    },
    EmbeddedEntry {
        id: "amre",
        game_code: "AMRE",
        json: include_str!("../data/amre.json"),
    },
    EmbeddedEntry {
        id: "amsj",
        game_code: "AMSJ",
        json: include_str!("../data/amsj.json"),
    },
    EmbeddedEntry {
        id: "amte",
        game_code: "AMTE",
        json: include_str!("../data/amte.json"),
    },
    EmbeddedEntry {
        id: "amtp",
        game_code: "AMTP",
        json: include_str!("../data/amtp.json"),
    },
    EmbeddedEntry {
        id: "amvj",
        game_code: "AMVJ",
        json: include_str!("../data/amvj.json"),
    },
    EmbeddedEntry {
        id: "amwe",
        game_code: "AMWE",
        json: include_str!("../data/amwe.json"),
    },
    EmbeddedEntry {
        id: "amyp",
        game_code: "AMYP",
        json: include_str!("../data/amyp.json"),
    },
    EmbeddedEntry {
        id: "an3e",
        game_code: "AN3E",
        json: include_str!("../data/an3e.json"),
    },
    EmbeddedEntry {
        id: "an4e",
        game_code: "AN4E",
        json: include_str!("../data/an4e.json"),
    },
    EmbeddedEntry {
        id: "an6e",
        game_code: "AN6E",
        json: include_str!("../data/an6e.json"),
    },
    EmbeddedEntry {
        id: "an8p",
        game_code: "AN8P",
        json: include_str!("../data/an8p.json"),
    },
    EmbeddedEntry {
        id: "anaj",
        game_code: "ANAJ",
        json: include_str!("../data/anaj.json"),
    },
    EmbeddedEntry {
        id: "ance",
        game_code: "ANCE",
        json: include_str!("../data/ance.json"),
    },
    EmbeddedEntry {
        id: "anfj",
        game_code: "ANFJ",
        json: include_str!("../data/anfj.json"),
    },
    EmbeddedEntry {
        id: "anje",
        game_code: "ANJE",
        json: include_str!("../data/anje.json"),
    },
    EmbeddedEntry {
        id: "anle",
        game_code: "ANLE",
        json: include_str!("../data/anle.json"),
    },
    EmbeddedEntry {
        id: "anpj",
        game_code: "ANPJ",
        json: include_str!("../data/anpj.json"),
    },
    EmbeddedEntry {
        id: "anqe",
        game_code: "ANQE",
        json: include_str!("../data/anqe.json"),
    },
    EmbeddedEntry {
        id: "ansj",
        game_code: "ANSJ",
        json: include_str!("../data/ansj.json"),
    },
    EmbeddedEntry {
        id: "antj",
        game_code: "ANTJ",
        json: include_str!("../data/antj.json"),
    },
    EmbeddedEntry {
        id: "anvj",
        game_code: "ANVJ",
        json: include_str!("../data/anvj.json"),
    },
    EmbeddedEntry {
        id: "anxp",
        game_code: "ANXP",
        json: include_str!("../data/anxp.json"),
    },
    EmbeddedEntry {
        id: "anyj",
        game_code: "ANYJ",
        json: include_str!("../data/anyj.json"),
    },
    EmbeddedEntry {
        id: "ao4e",
        game_code: "AO4E",
        json: include_str!("../data/ao4e.json"),
    },
    EmbeddedEntry {
        id: "aoex",
        game_code: "AOEX",
        json: include_str!("../data/aoex.json"),
    },
    EmbeddedEntry {
        id: "aoge",
        game_code: "AOGE",
        json: include_str!("../data/aoge.json"),
    },
    EmbeddedEntry {
        id: "aoie",
        game_code: "AOIE",
        json: include_str!("../data/aoie.json"),
    },
    EmbeddedEntry {
        id: "aome",
        game_code: "AOME",
        json: include_str!("../data/aome.json"),
    },
    EmbeddedEntry {
        id: "aosj",
        game_code: "AOSJ",
        json: include_str!("../data/aosj.json"),
    },
    EmbeddedEntry {
        id: "aowe",
        game_code: "AOWE",
        json: include_str!("../data/aowe.json"),
    },
    EmbeddedEntry {
        id: "ap3j",
        game_code: "AP3J",
        json: include_str!("../data/ap3j.json"),
    },
    EmbeddedEntry {
        id: "ap4j",
        game_code: "AP4J",
        json: include_str!("../data/ap4j.json"),
    },
    EmbeddedEntry {
        id: "ap5p",
        game_code: "AP5P",
        json: include_str!("../data/ap5p.json"),
    },
    EmbeddedEntry {
        id: "ap6j",
        game_code: "AP6J",
        json: include_str!("../data/ap6j.json"),
    },
    EmbeddedEntry {
        id: "ap8p",
        game_code: "AP8P",
        json: include_str!("../data/ap8p.json"),
    },
    EmbeddedEntry {
        id: "apbj",
        game_code: "APBJ",
        json: include_str!("../data/apbj.json"),
    },
    EmbeddedEntry {
        id: "apdp",
        game_code: "APDP",
        json: include_str!("../data/apdp.json"),
    },
    EmbeddedEntry {
        id: "aplp",
        game_code: "APLP",
        json: include_str!("../data/aplp.json"),
    },
    EmbeddedEntry {
        id: "aptp",
        game_code: "APTP",
        json: include_str!("../data/aptp.json"),
    },
    EmbeddedEntry {
        id: "apuj",
        game_code: "APUJ",
        json: include_str!("../data/apuj.json"),
    },
    EmbeddedEntry {
        id: "apxp",
        game_code: "APXP",
        json: include_str!("../data/apxp.json"),
    },
    EmbeddedEntry {
        id: "apyj",
        game_code: "APYJ",
        json: include_str!("../data/apyj.json"),
    },
    EmbeddedEntry {
        id: "aq2j",
        game_code: "AQ2J",
        json: include_str!("../data/aq2j.json"),
    },
    EmbeddedEntry {
        id: "aqap",
        game_code: "AQAP",
        json: include_str!("../data/aqap.json"),
    },
    EmbeddedEntry {
        id: "aqmp",
        game_code: "AQMP",
        json: include_str!("../data/aqmp.json"),
    },
    EmbeddedEntry {
        id: "aqte",
        game_code: "AQTE",
        json: include_str!("../data/aqte.json"),
    },
    EmbeddedEntry {
        id: "aqwe",
        game_code: "AQWE",
        json: include_str!("../data/aqwe.json"),
    },
    EmbeddedEntry {
        id: "aqxe",
        game_code: "AQXE",
        json: include_str!("../data/aqxe.json"),
    },
    EmbeddedEntry {
        id: "ar6e",
        game_code: "AR6E",
        json: include_str!("../data/ar6e.json"),
    },
    EmbeddedEntry {
        id: "aree",
        game_code: "AREE",
        json: include_str!("../data/aree.json"),
    },
    EmbeddedEntry {
        id: "arpe",
        game_code: "ARPE",
        json: include_str!("../data/arpe.json"),
    },
    EmbeddedEntry {
        id: "arsp",
        game_code: "ARSP",
        json: include_str!("../data/arsp.json"),
    },
    EmbeddedEntry {
        id: "arvj",
        game_code: "ARVJ",
        json: include_str!("../data/arvj.json"),
    },
    EmbeddedEntry {
        id: "arye",
        game_code: "ARYE",
        json: include_str!("../data/arye.json"),
    },
    EmbeddedEntry {
        id: "arzj",
        game_code: "ARZJ",
        json: include_str!("../data/arzj.json"),
    },
    EmbeddedEntry {
        id: "as2e",
        game_code: "AS2E",
        json: include_str!("../data/as2e.json"),
    },
    EmbeddedEntry {
        id: "as4e",
        game_code: "AS4E",
        json: include_str!("../data/as4e.json"),
    },
    EmbeddedEntry {
        id: "asbj",
        game_code: "ASBJ",
        json: include_str!("../data/asbj.json"),
    },
    EmbeddedEntry {
        id: "ashj",
        game_code: "ASHJ",
        json: include_str!("../data/ashj.json"),
    },
    EmbeddedEntry {
        id: "asie",
        game_code: "ASIE",
        json: include_str!("../data/asie.json"),
    },
    EmbeddedEntry {
        id: "asnb",
        game_code: "ASNB",
        json: include_str!("../data/asnb.json"),
    },
    EmbeddedEntry {
        id: "asoj",
        game_code: "ASOJ",
        json: include_str!("../data/asoj.json"),
    },
    EmbeddedEntry {
        id: "asop",
        game_code: "ASOP",
        json: include_str!("../data/asop.json"),
    },
    EmbeddedEntry {
        id: "asse",
        game_code: "ASSE",
        json: include_str!("../data/asse.json"),
    },
    EmbeddedEntry {
        id: "astc",
        game_code: "ASTC",
        json: include_str!("../data/astc.json"),
    },
    EmbeddedEntry {
        id: "asye",
        game_code: "ASYE",
        json: include_str!("../data/asye.json"),
    },
    EmbeddedEntry {
        id: "at3f",
        game_code: "AT3F",
        json: include_str!("../data/at3f.json"),
    },
    EmbeddedEntry {
        id: "at5e",
        game_code: "AT5E",
        json: include_str!("../data/at5e.json"),
    },
    EmbeddedEntry {
        id: "at8p",
        game_code: "AT8P",
        json: include_str!("../data/at8p.json"),
    },
    EmbeddedEntry {
        id: "at9e",
        game_code: "AT9E",
        json: include_str!("../data/at9e.json"),
    },
    EmbeddedEntry {
        id: "atae",
        game_code: "ATAE",
        json: include_str!("../data/atae.json"),
    },
    EmbeddedEntry {
        id: "atce",
        game_code: "ATCE",
        json: include_str!("../data/atce.json"),
    },
    EmbeddedEntry {
        id: "atee",
        game_code: "ATEE",
        json: include_str!("../data/atee.json"),
    },
    EmbeddedEntry {
        id: "athe",
        game_code: "ATHE",
        json: include_str!("../data/athe.json"),
    },
    EmbeddedEntry {
        id: "atke",
        game_code: "ATKE",
        json: include_str!("../data/atke.json"),
    },
    EmbeddedEntry {
        id: "atme",
        game_code: "ATME",
        json: include_str!("../data/atme.json"),
    },
    EmbeddedEntry {
        id: "atoe",
        game_code: "ATOE",
        json: include_str!("../data/atoe.json"),
    },
    EmbeddedEntry {
        id: "atqp",
        game_code: "ATQP",
        json: include_str!("../data/atqp.json"),
    },
    EmbeddedEntry {
        id: "atup",
        game_code: "ATUP",
        json: include_str!("../data/atup.json"),
    },
    EmbeddedEntry {
        id: "atxp",
        game_code: "ATXP",
        json: include_str!("../data/atxp.json"),
    },
    EmbeddedEntry {
        id: "au2e",
        game_code: "AU2E",
        json: include_str!("../data/au2e.json"),
    },
    EmbeddedEntry {
        id: "auej",
        game_code: "AUEJ",
        json: include_str!("../data/auej.json"),
    },
    EmbeddedEntry {
        id: "ausj",
        game_code: "AUSJ",
        json: include_str!("../data/ausj.json"),
    },
    EmbeddedEntry {
        id: "auxe",
        game_code: "AUXE",
        json: include_str!("../data/auxe.json"),
    },
    EmbeddedEntry {
        id: "auyj",
        game_code: "AUYJ",
        json: include_str!("../data/auyj.json"),
    },
    EmbeddedEntry {
        id: "av3e",
        game_code: "AV3E",
        json: include_str!("../data/av3e.json"),
    },
    EmbeddedEntry {
        id: "avee",
        game_code: "AVEE",
        json: include_str!("../data/avee.json"),
    },
    EmbeddedEntry {
        id: "avfj",
        game_code: "AVFJ",
        json: include_str!("../data/avfj.json"),
    },
    EmbeddedEntry {
        id: "avij",
        game_code: "AVIJ",
        json: include_str!("../data/avij.json"),
    },
    EmbeddedEntry {
        id: "avkp",
        game_code: "AVKP",
        json: include_str!("../data/avkp.json"),
    },
    EmbeddedEntry {
        id: "avse",
        game_code: "AVSE",
        json: include_str!("../data/avse.json"),
    },
    EmbeddedEntry {
        id: "avtp",
        game_code: "AVTP",
        json: include_str!("../data/avtp.json"),
    },
    EmbeddedEntry {
        id: "avyd",
        game_code: "AVYD",
        json: include_str!("../data/avyd.json"),
    },
    EmbeddedEntry {
        id: "avzp",
        game_code: "AVZP",
        json: include_str!("../data/avzp.json"),
    },
    EmbeddedEntry {
        id: "aw2e",
        game_code: "AW2E",
        json: include_str!("../data/aw2e.json"),
    },
    EmbeddedEntry {
        id: "awae",
        game_code: "AWAE",
        json: include_str!("../data/awae.json"),
    },
    EmbeddedEntry {
        id: "awde",
        game_code: "AWDE",
        json: include_str!("../data/awde.json"),
    },
    EmbeddedEntry {
        id: "awie",
        game_code: "AWIE",
        json: include_str!("../data/awie.json"),
    },
    EmbeddedEntry {
        id: "awpj",
        game_code: "AWPJ",
        json: include_str!("../data/awpj.json"),
    },
    EmbeddedEntry {
        id: "awrp",
        game_code: "AWRP",
        json: include_str!("../data/awrp.json"),
    },
    EmbeddedEntry {
        id: "awup",
        game_code: "AWUP",
        json: include_str!("../data/awup.json"),
    },
    EmbeddedEntry {
        id: "awve",
        game_code: "AWVE",
        json: include_str!("../data/awve.json"),
    },
    EmbeddedEntry {
        id: "awwe",
        game_code: "AWWE",
        json: include_str!("../data/awwe.json"),
    },
    EmbeddedEntry {
        id: "awxe",
        game_code: "AWXE",
        json: include_str!("../data/awxe.json"),
    },
    EmbeddedEntry {
        id: "awzj",
        game_code: "AWZJ",
        json: include_str!("../data/awzj.json"),
    },
    EmbeddedEntry {
        id: "ax4e",
        game_code: "AX4E",
        json: include_str!("../data/ax4e.json"),
    },
    EmbeddedEntry {
        id: "ax4p",
        game_code: "AX4P",
        json: include_str!("../data/ax4p.json"),
    },
    EmbeddedEntry {
        id: "axdp",
        game_code: "AXDP",
        json: include_str!("../data/axdp.json"),
    },
    EmbeddedEntry {
        id: "axme",
        game_code: "AXME",
        json: include_str!("../data/axme.json"),
    },
    EmbeddedEntry {
        id: "axpe",
        game_code: "AXPE",
        json: include_str!("../data/axpe.json"),
    },
    EmbeddedEntry {
        id: "axsp",
        game_code: "AXSP",
        json: include_str!("../data/axsp.json"),
    },
    EmbeddedEntry {
        id: "axve",
        game_code: "AXVE",
        json: include_str!("../data/axve.json"),
    },
    EmbeddedEntry {
        id: "axxp",
        game_code: "AXXP",
        json: include_str!("../data/axxp.json"),
    },
    EmbeddedEntry {
        id: "axye",
        game_code: "AXYE",
        json: include_str!("../data/axye.json"),
    },
    EmbeddedEntry {
        id: "ay5e",
        game_code: "AY5E",
        json: include_str!("../data/ay5e.json"),
    },
    EmbeddedEntry {
        id: "ay6j",
        game_code: "AY6J",
        json: include_str!("../data/ay6j.json"),
    },
    EmbeddedEntry {
        id: "ay7j",
        game_code: "AY7J",
        json: include_str!("../data/ay7j.json"),
    },
    EmbeddedEntry {
        id: "ay8e",
        game_code: "AY8E",
        json: include_str!("../data/ay8e.json"),
    },
    EmbeddedEntry {
        id: "ayaj",
        game_code: "AYAJ",
        json: include_str!("../data/ayaj.json"),
    },
    EmbeddedEntry {
        id: "aybe",
        game_code: "AYBE",
        json: include_str!("../data/aybe.json"),
    },
    EmbeddedEntry {
        id: "ayde",
        game_code: "AYDE",
        json: include_str!("../data/ayde.json"),
    },
    EmbeddedEntry {
        id: "ayge",
        game_code: "AYGE",
        json: include_str!("../data/ayge.json"),
    },
    EmbeddedEntry {
        id: "ayhe",
        game_code: "AYHE",
        json: include_str!("../data/ayhe.json"),
    },
    EmbeddedEntry {
        id: "ayke",
        game_code: "AYKE",
        json: include_str!("../data/ayke.json"),
    },
    EmbeddedEntry {
        id: "ayle",
        game_code: "AYLE",
        json: include_str!("../data/ayle.json"),
    },
    EmbeddedEntry {
        id: "aynp",
        game_code: "AYNP",
        json: include_str!("../data/aynp.json"),
    },
    EmbeddedEntry {
        id: "aywp",
        game_code: "AYWP",
        json: include_str!("../data/aywp.json"),
    },
    EmbeddedEntry {
        id: "ayze",
        game_code: "AYZE",
        json: include_str!("../data/ayze.json"),
    },
    EmbeddedEntry {
        id: "az2e",
        game_code: "AZ2E",
        json: include_str!("../data/az2e.json"),
    },
    EmbeddedEntry {
        id: "az8e",
        game_code: "AZ8E",
        json: include_str!("../data/az8e.json"),
    },
    EmbeddedEntry {
        id: "az9j",
        game_code: "AZ9J",
        json: include_str!("../data/az9j.json"),
    },
    EmbeddedEntry {
        id: "azce",
        game_code: "AZCE",
        json: include_str!("../data/azce.json"),
    },
    EmbeddedEntry {
        id: "azfp",
        game_code: "AZFP",
        json: include_str!("../data/azfp.json"),
    },
    EmbeddedEntry {
        id: "azjj",
        game_code: "AZJJ",
        json: include_str!("../data/azjj.json"),
    },
    EmbeddedEntry {
        id: "azle",
        game_code: "AZLE",
        json: include_str!("../data/azle.json"),
    },
    EmbeddedEntry {
        id: "azlp",
        game_code: "AZLP",
        json: include_str!("../data/azlp.json"),
    },
    EmbeddedEntry {
        id: "azpe",
        game_code: "AZPE",
        json: include_str!("../data/azpe.json"),
    },
    EmbeddedEntry {
        id: "azup",
        game_code: "AZUP",
        json: include_str!("../data/azup.json"),
    },
    EmbeddedEntry {
        id: "azwp",
        game_code: "AZWP",
        json: include_str!("../data/azwp.json"),
    },
    EmbeddedEntry {
        id: "b24e",
        game_code: "B24E",
        json: include_str!("../data/b24e.json"),
    },
    EmbeddedEntry {
        id: "b27p",
        game_code: "B27P",
        json: include_str!("../data/b27p.json"),
    },
    EmbeddedEntry {
        id: "b2de",
        game_code: "B2DE",
        json: include_str!("../data/b2de.json"),
    },
    EmbeddedEntry {
        id: "b2le",
        game_code: "B2LE",
        json: include_str!("../data/b2le.json"),
    },
    EmbeddedEntry {
        id: "b2mp",
        game_code: "B2MP",
        json: include_str!("../data/b2mp.json"),
    },
    EmbeddedEntry {
        id: "b2ne",
        game_code: "B2NE",
        json: include_str!("../data/b2ne.json"),
    },
    EmbeddedEntry {
        id: "b2qp",
        game_code: "B2QP",
        json: include_str!("../data/b2qp.json"),
    },
    EmbeddedEntry {
        id: "b2rj",
        game_code: "B2RJ",
        json: include_str!("../data/b2rj.json"),
    },
    EmbeddedEntry {
        id: "b35p",
        game_code: "B35P",
        json: include_str!("../data/b35p.json"),
    },
    EmbeddedEntry {
        id: "b36e",
        game_code: "B36E",
        json: include_str!("../data/b36e.json"),
    },
    EmbeddedEntry {
        id: "b3ae",
        game_code: "B3AE",
        json: include_str!("../data/b3ae.json"),
    },
    EmbeddedEntry {
        id: "b3dj",
        game_code: "B3DJ",
        json: include_str!("../data/b3dj.json"),
    },
    EmbeddedEntry {
        id: "b3hp",
        game_code: "B3HP",
        json: include_str!("../data/b3hp.json"),
    },
    EmbeddedEntry {
        id: "b3re",
        game_code: "B3RE",
        json: include_str!("../data/b3re.json"),
    },
    EmbeddedEntry {
        id: "b3se",
        game_code: "B3SE",
        json: include_str!("../data/b3se.json"),
    },
    EmbeddedEntry {
        id: "b3tj",
        game_code: "B3TJ",
        json: include_str!("../data/b3tj.json"),
    },
    EmbeddedEntry {
        id: "b3xe",
        game_code: "B3XE",
        json: include_str!("../data/b3xe.json"),
    },
    EmbeddedEntry {
        id: "b3yp",
        game_code: "B3YP",
        json: include_str!("../data/b3yp.json"),
    },
    EmbeddedEntry {
        id: "b3ze",
        game_code: "B3ZE",
        json: include_str!("../data/b3ze.json"),
    },
    EmbeddedEntry {
        id: "b4be",
        game_code: "B4BE",
        json: include_str!("../data/b4be.json"),
    },
    EmbeddedEntry {
        id: "b4gj",
        game_code: "B4GJ",
        json: include_str!("../data/b4gj.json"),
    },
    EmbeddedEntry {
        id: "b4ie",
        game_code: "B4IE",
        json: include_str!("../data/b4ie.json"),
    },
    EmbeddedEntry {
        id: "b4me",
        game_code: "B4ME",
        json: include_str!("../data/b4me.json"),
    },
    EmbeddedEntry {
        id: "b4ox",
        game_code: "B4OX",
        json: include_str!("../data/b4ox.json"),
    },
    EmbeddedEntry {
        id: "b4sj",
        game_code: "B4SJ",
        json: include_str!("../data/b4sj.json"),
    },
    EmbeddedEntry {
        id: "b4ue",
        game_code: "B4UE",
        json: include_str!("../data/b4ue.json"),
    },
    EmbeddedEntry {
        id: "b4we",
        game_code: "B4WE",
        json: include_str!("../data/b4we.json"),
    },
    EmbeddedEntry {
        id: "b4ze",
        game_code: "B4ZE",
        json: include_str!("../data/b4ze.json"),
    },
    EmbeddedEntry {
        id: "b53e",
        game_code: "B53E",
        json: include_str!("../data/b53e.json"),
    },
    EmbeddedEntry {
        id: "b63p",
        game_code: "B63P",
        json: include_str!("../data/b63p.json"),
    },
    EmbeddedEntry {
        id: "b6ze",
        game_code: "B6ZE",
        json: include_str!("../data/b6ze.json"),
    },
    EmbeddedEntry {
        id: "b82e",
        game_code: "B82E",
        json: include_str!("../data/b82e.json"),
    },
    EmbeddedEntry {
        id: "b83e",
        game_code: "B83E",
        json: include_str!("../data/b83e.json"),
    },
    EmbeddedEntry {
        id: "b85a",
        game_code: "B85A",
        json: include_str!("../data/b85a.json"),
    },
    EmbeddedEntry {
        id: "b88e",
        game_code: "B88E",
        json: include_str!("../data/b88e.json"),
    },
    EmbeddedEntry {
        id: "b8ce",
        game_code: "B8CE",
        json: include_str!("../data/b8ce.json"),
    },
    EmbeddedEntry {
        id: "b8ee",
        game_code: "B8EE",
        json: include_str!("../data/b8ee.json"),
    },
    EmbeddedEntry {
        id: "b8fe",
        game_code: "B8FE",
        json: include_str!("../data/b8fe.json"),
    },
    EmbeddedEntry {
        id: "b8ke",
        game_code: "B8KE",
        json: include_str!("../data/b8ke.json"),
    },
    EmbeddedEntry {
        id: "b8me",
        game_code: "B8ME",
        json: include_str!("../data/b8me.json"),
    },
    EmbeddedEntry {
        id: "b8pj",
        game_code: "B8PJ",
        json: include_str!("../data/b8pj.json"),
    },
    EmbeddedEntry {
        id: "b8qe",
        game_code: "B8QE",
        json: include_str!("../data/b8qe.json"),
    },
    EmbeddedEntry {
        id: "b8se",
        game_code: "B8SE",
        json: include_str!("../data/b8se.json"),
    },
    EmbeddedEntry {
        id: "b9aj",
        game_code: "B9AJ",
        json: include_str!("../data/b9aj.json"),
    },
    EmbeddedEntry {
        id: "b9bj",
        game_code: "B9BJ",
        json: include_str!("../data/b9bj.json"),
    },
    EmbeddedEntry {
        id: "b9cj",
        game_code: "B9CJ",
        json: include_str!("../data/b9cj.json"),
    },
    EmbeddedEntry {
        id: "bade",
        game_code: "BADE",
        json: include_str!("../data/bade.json"),
    },
    EmbeddedEntry {
        id: "badp",
        game_code: "BADP",
        json: include_str!("../data/badp.json"),
    },
    EmbeddedEntry {
        id: "bahp",
        game_code: "BAHP",
        json: include_str!("../data/bahp.json"),
    },
    EmbeddedEntry {
        id: "baje",
        game_code: "BAJE",
        json: include_str!("../data/baje.json"),
    },
    EmbeddedEntry {
        id: "bale",
        game_code: "BALE",
        json: include_str!("../data/bale.json"),
    },
    EmbeddedEntry {
        id: "baqp",
        game_code: "BAQP",
        json: include_str!("../data/baqp.json"),
    },
    EmbeddedEntry {
        id: "bave",
        game_code: "BAVE",
        json: include_str!("../data/bave.json"),
    },
    EmbeddedEntry {
        id: "bawe",
        game_code: "BAWE",
        json: include_str!("../data/bawe.json"),
    },
    EmbeddedEntry {
        id: "bazj",
        game_code: "BAZJ",
        json: include_str!("../data/bazj.json"),
    },
    EmbeddedEntry {
        id: "bb2e",
        game_code: "BB2E",
        json: include_str!("../data/bb2e.json"),
    },
    EmbeddedEntry {
        id: "bb7e",
        game_code: "BB7E",
        json: include_str!("../data/bb7e.json"),
    },
    EmbeddedEntry {
        id: "bbae",
        game_code: "BBAE",
        json: include_str!("../data/bbae.json"),
    },
    EmbeddedEntry {
        id: "bbie",
        game_code: "BBIE",
        json: include_str!("../data/bbie.json"),
    },
    EmbeddedEntry {
        id: "bbkp",
        game_code: "BBKP",
        json: include_str!("../data/bbkp.json"),
    },
    EmbeddedEntry {
        id: "bbmj",
        game_code: "BBMJ",
        json: include_str!("../data/bbmj.json"),
    },
    EmbeddedEntry {
        id: "bbne",
        game_code: "BBNE",
        json: include_str!("../data/bbne.json"),
    },
    EmbeddedEntry {
        id: "bbqj",
        game_code: "BBQJ",
        json: include_str!("../data/bbqj.json"),
    },
    EmbeddedEntry {
        id: "bbre",
        game_code: "BBRE",
        json: include_str!("../data/bbre.json"),
    },
    EmbeddedEntry {
        id: "bbsj",
        game_code: "BBSJ",
        json: include_str!("../data/bbsj.json"),
    },
    EmbeddedEntry {
        id: "bbve",
        game_code: "BBVE",
        json: include_str!("../data/bbve.json"),
    },
    EmbeddedEntry {
        id: "bbyx",
        game_code: "BBYX",
        json: include_str!("../data/bbyx.json"),
    },
    EmbeddedEntry {
        id: "bc2j",
        game_code: "BC2J",
        json: include_str!("../data/bc2j.json"),
    },
    EmbeddedEntry {
        id: "bc9e",
        game_code: "BC9E",
        json: include_str!("../data/bc9e.json"),
    },
    EmbeddedEntry {
        id: "bcae",
        game_code: "BCAE",
        json: include_str!("../data/bcae.json"),
    },
    EmbeddedEntry {
        id: "bcce",
        game_code: "BCCE",
        json: include_str!("../data/bcce.json"),
    },
    EmbeddedEntry {
        id: "bcdp",
        game_code: "BCDP",
        json: include_str!("../data/bcdp.json"),
    },
    EmbeddedEntry {
        id: "bcfe",
        game_code: "BCFE",
        json: include_str!("../data/bcfe.json"),
    },
    EmbeddedEntry {
        id: "bcgp",
        game_code: "BCGP",
        json: include_str!("../data/bcgp.json"),
    },
    EmbeddedEntry {
        id: "bche",
        game_code: "BCHE",
        json: include_str!("../data/bche.json"),
    },
    EmbeddedEntry {
        id: "bcjp",
        game_code: "BCJP",
        json: include_str!("../data/bcjp.json"),
    },
    EmbeddedEntry {
        id: "bcke",
        game_code: "BCKE",
        json: include_str!("../data/bcke.json"),
    },
    EmbeddedEntry {
        id: "bcme",
        game_code: "BCME",
        json: include_str!("../data/bcme.json"),
    },
    EmbeddedEntry {
        id: "bcne",
        game_code: "BCNE",
        json: include_str!("../data/bcne.json"),
    },
    EmbeddedEntry {
        id: "bcve",
        game_code: "BCVE",
        json: include_str!("../data/bcve.json"),
    },
    EmbeddedEntry {
        id: "bcze",
        game_code: "BCZE",
        json: include_str!("../data/bcze.json"),
    },
    EmbeddedEntry {
        id: "bd4e",
        game_code: "BD4E",
        json: include_str!("../data/bd4e.json"),
    },
    EmbeddedEntry {
        id: "bd6e",
        game_code: "BD6E",
        json: include_str!("../data/bd6e.json"),
    },
    EmbeddedEntry {
        id: "bdbe",
        game_code: "BDBE",
        json: include_str!("../data/bdbe.json"),
    },
    EmbeddedEntry {
        id: "bdge",
        game_code: "BDGE",
        json: include_str!("../data/bdge.json"),
    },
    EmbeddedEntry {
        id: "bdqe",
        game_code: "BDQE",
        json: include_str!("../data/bdqe.json"),
    },
    EmbeddedEntry {
        id: "bdsp",
        game_code: "BDSP",
        json: include_str!("../data/bdsp.json"),
    },
    EmbeddedEntry {
        id: "bdte",
        game_code: "BDTE",
        json: include_str!("../data/bdte.json"),
    },
    EmbeddedEntry {
        id: "bdue",
        game_code: "BDUE",
        json: include_str!("../data/bdue.json"),
    },
    EmbeddedEntry {
        id: "bdve",
        game_code: "BDVE",
        json: include_str!("../data/bdve.json"),
    },
    EmbeddedEntry {
        id: "bdxj",
        game_code: "BDXJ",
        json: include_str!("../data/bdxj.json"),
    },
    EmbeddedEntry {
        id: "be3e",
        game_code: "BE3E",
        json: include_str!("../data/be3e.json"),
    },
    EmbeddedEntry {
        id: "be8e",
        game_code: "BE8E",
        json: include_str!("../data/be8e.json"),
    },
    EmbeddedEntry {
        id: "beae",
        game_code: "BEAE",
        json: include_str!("../data/beae.json"),
    },
    EmbeddedEntry {
        id: "bedp",
        game_code: "BEDP",
        json: include_str!("../data/bedp.json"),
    },
    EmbeddedEntry {
        id: "benp",
        game_code: "BENP",
        json: include_str!("../data/benp.json"),
    },
    EmbeddedEntry {
        id: "bese",
        game_code: "BESE",
        json: include_str!("../data/bese.json"),
    },
    EmbeddedEntry {
        id: "bexp",
        game_code: "BEXP",
        json: include_str!("../data/bexp.json"),
    },
    EmbeddedEntry {
        id: "bf4p",
        game_code: "BF4P",
        json: include_str!("../data/bf4p.json"),
    },
    EmbeddedEntry {
        id: "bfdj",
        game_code: "BFDJ",
        json: include_str!("../data/bfdj.json"),
    },
    EmbeddedEntry {
        id: "bfep",
        game_code: "BFEP",
        json: include_str!("../data/bfep.json"),
    },
    EmbeddedEntry {
        id: "bffe",
        game_code: "BFFE",
        json: include_str!("../data/bffe.json"),
    },
    EmbeddedEntry {
        id: "bfge",
        game_code: "BFGE",
        json: include_str!("../data/bfge.json"),
    },
    EmbeddedEntry {
        id: "bfje",
        game_code: "BFJE",
        json: include_str!("../data/bfje.json"),
    },
    EmbeddedEntry {
        id: "bfkp",
        game_code: "BFKP",
        json: include_str!("../data/bfkp.json"),
    },
    EmbeddedEntry {
        id: "bflp",
        game_code: "BFLP",
        json: include_str!("../data/bflp.json"),
    },
    EmbeddedEntry {
        id: "bfpj",
        game_code: "BFPJ",
        json: include_str!("../data/bfpj.json"),
    },
    EmbeddedEntry {
        id: "bftj",
        game_code: "BFTJ",
        json: include_str!("../data/bftj.json"),
    },
    EmbeddedEntry {
        id: "bfyp",
        game_code: "BFYP",
        json: include_str!("../data/bfyp.json"),
    },
    EmbeddedEntry {
        id: "bfzj",
        game_code: "BFZJ",
        json: include_str!("../data/bfzj.json"),
    },
    EmbeddedEntry {
        id: "bg9e",
        game_code: "BG9E",
        json: include_str!("../data/bg9e.json"),
    },
    EmbeddedEntry {
        id: "bgce",
        game_code: "BGCE",
        json: include_str!("../data/bgce.json"),
    },
    EmbeddedEntry {
        id: "bgdp",
        game_code: "BGDP",
        json: include_str!("../data/bgdp.json"),
    },
    EmbeddedEntry {
        id: "bghj",
        game_code: "BGHJ",
        json: include_str!("../data/bghj.json"),
    },
    EmbeddedEntry {
        id: "bgjj",
        game_code: "BGJJ",
        json: include_str!("../data/bgjj.json"),
    },
    EmbeddedEntry {
        id: "bgqe",
        game_code: "BGQE",
        json: include_str!("../data/bgqe.json"),
    },
    EmbeddedEntry {
        id: "bgte",
        game_code: "BGTE",
        json: include_str!("../data/bgte.json"),
    },
    EmbeddedEntry {
        id: "bgwj",
        game_code: "BGWJ",
        json: include_str!("../data/bgwj.json"),
    },
    EmbeddedEntry {
        id: "bgxj",
        game_code: "BGXJ",
        json: include_str!("../data/bgxj.json"),
    },
    EmbeddedEntry {
        id: "bgyj",
        game_code: "BGYJ",
        json: include_str!("../data/bgyj.json"),
    },
    EmbeddedEntry {
        id: "bgze",
        game_code: "BGZE",
        json: include_str!("../data/bgze.json"),
    },
    EmbeddedEntry {
        id: "bh3p",
        game_code: "BH3P",
        json: include_str!("../data/bh3p.json"),
    },
    EmbeddedEntry {
        id: "bh4e",
        game_code: "BH4E",
        json: include_str!("../data/bh4e.json"),
    },
    EmbeddedEntry {
        id: "bh5e",
        game_code: "BH5E",
        json: include_str!("../data/bh5e.json"),
    },
    EmbeddedEntry {
        id: "bh6j",
        game_code: "BH6J",
        json: include_str!("../data/bh6j.json"),
    },
    EmbeddedEntry {
        id: "bh7e",
        game_code: "BH7E",
        json: include_str!("../data/bh7e.json"),
    },
    EmbeddedEntry {
        id: "bh9p",
        game_code: "BH9P",
        json: include_str!("../data/bh9p.json"),
    },
    EmbeddedEntry {
        id: "bhbp",
        game_code: "BHBP",
        json: include_str!("../data/bhbp.json"),
    },
    EmbeddedEntry {
        id: "bhdj",
        game_code: "BHDJ",
        json: include_str!("../data/bhdj.json"),
    },
    EmbeddedEntry {
        id: "bhfj",
        game_code: "BHFJ",
        json: include_str!("../data/bhfj.json"),
    },
    EmbeddedEntry {
        id: "bhge",
        game_code: "BHGE",
        json: include_str!("../data/bhge.json"),
    },
    EmbeddedEntry {
        id: "bhhe",
        game_code: "BHHE",
        json: include_str!("../data/bhhe.json"),
    },
    EmbeddedEntry {
        id: "bhpj",
        game_code: "BHPJ",
        json: include_str!("../data/bhpj.json"),
    },
    EmbeddedEntry {
        id: "bhte",
        game_code: "BHTE",
        json: include_str!("../data/bhte.json"),
    },
    EmbeddedEntry {
        id: "bhve",
        game_code: "BHVE",
        json: include_str!("../data/bhve.json"),
    },
    EmbeddedEntry {
        id: "bhyj",
        game_code: "BHYJ",
        json: include_str!("../data/bhyj.json"),
    },
    EmbeddedEntry {
        id: "bi3e",
        game_code: "BI3E",
        json: include_str!("../data/bi3e.json"),
    },
    EmbeddedEntry {
        id: "biap",
        game_code: "BIAP",
        json: include_str!("../data/biap.json"),
    },
    EmbeddedEntry {
        id: "biee",
        game_code: "BIEE",
        json: include_str!("../data/biee.json"),
    },
    EmbeddedEntry {
        id: "bihp",
        game_code: "BIHP",
        json: include_str!("../data/bihp.json"),
    },
    EmbeddedEntry {
        id: "biie",
        game_code: "BIIE",
        json: include_str!("../data/biie.json"),
    },
    EmbeddedEntry {
        id: "bile",
        game_code: "BILE",
        json: include_str!("../data/bile.json"),
    },
    EmbeddedEntry {
        id: "bj2e",
        game_code: "BJ2E",
        json: include_str!("../data/bj2e.json"),
    },
    EmbeddedEntry {
        id: "bjbj",
        game_code: "BJBJ",
        json: include_str!("../data/bjbj.json"),
    },
    EmbeddedEntry {
        id: "bjge",
        game_code: "BJGE",
        json: include_str!("../data/bjge.json"),
    },
    EmbeddedEntry {
        id: "bjke",
        game_code: "BJKE",
        json: include_str!("../data/bjke.json"),
    },
    EmbeddedEntry {
        id: "bjue",
        game_code: "BJUE",
        json: include_str!("../data/bjue.json"),
    },
    EmbeddedEntry {
        id: "bjwe",
        game_code: "BJWE",
        json: include_str!("../data/bjwe.json"),
    },
    EmbeddedEntry {
        id: "bjxe",
        game_code: "BJXE",
        json: include_str!("../data/bjxe.json"),
    },
    EmbeddedEntry {
        id: "bjye",
        game_code: "BJYE",
        json: include_str!("../data/bjye.json"),
    },
    EmbeddedEntry {
        id: "bk4j",
        game_code: "BK4J",
        json: include_str!("../data/bk4j.json"),
    },
    EmbeddedEntry {
        id: "bk6j",
        game_code: "BK6J",
        json: include_str!("../data/bk6j.json"),
    },
    EmbeddedEntry {
        id: "bkcj",
        game_code: "BKCJ",
        json: include_str!("../data/bkcj.json"),
    },
    EmbeddedEntry {
        id: "bkfx",
        game_code: "BKFX",
        json: include_str!("../data/bkfx.json"),
    },
    EmbeddedEntry {
        id: "bkhp",
        game_code: "BKHP",
        json: include_str!("../data/bkhp.json"),
    },
    EmbeddedEntry {
        id: "bkme",
        game_code: "BKME",
        json: include_str!("../data/bkme.json"),
    },
    EmbeddedEntry {
        id: "bkqp",
        game_code: "BKQP",
        json: include_str!("../data/bkqp.json"),
    },
    EmbeddedEntry {
        id: "bksj",
        game_code: "BKSJ",
        json: include_str!("../data/bksj.json"),
    },
    EmbeddedEntry {
        id: "bkte",
        game_code: "BKTE",
        json: include_str!("../data/bkte.json"),
    },
    EmbeddedEntry {
        id: "bkwe",
        game_code: "BKWE",
        json: include_str!("../data/bkwe.json"),
    },
    EmbeddedEntry {
        id: "bkze",
        game_code: "BKZE",
        json: include_str!("../data/bkze.json"),
    },
    EmbeddedEntry {
        id: "bl7e",
        game_code: "BL7E",
        json: include_str!("../data/bl7e.json"),
    },
    EmbeddedEntry {
        id: "bl8e",
        game_code: "BL8E",
        json: include_str!("../data/bl8e.json"),
    },
    EmbeddedEntry {
        id: "blcp",
        game_code: "BLCP",
        json: include_str!("../data/blcp.json"),
    },
    EmbeddedEntry {
        id: "blfe",
        game_code: "BLFE",
        json: include_str!("../data/blfe.json"),
    },
    EmbeddedEntry {
        id: "blke",
        game_code: "BLKE",
        json: include_str!("../data/blke.json"),
    },
    EmbeddedEntry {
        id: "blme",
        game_code: "BLME",
        json: include_str!("../data/blme.json"),
    },
    EmbeddedEntry {
        id: "blne",
        game_code: "BLNE",
        json: include_str!("../data/blne.json"),
    },
    EmbeddedEntry {
        id: "bloe",
        game_code: "BLOE",
        json: include_str!("../data/bloe.json"),
    },
    EmbeddedEntry {
        id: "blrj",
        game_code: "BLRJ",
        json: include_str!("../data/blrj.json"),
    },
    EmbeddedEntry {
        id: "blsp",
        game_code: "BLSP",
        json: include_str!("../data/blsp.json"),
    },
    EmbeddedEntry {
        id: "blte",
        game_code: "BLTE",
        json: include_str!("../data/blte.json"),
    },
    EmbeddedEntry {
        id: "blwe",
        game_code: "BLWE",
        json: include_str!("../data/blwe.json"),
    },
    EmbeddedEntry {
        id: "blxp",
        game_code: "BLXP",
        json: include_str!("../data/blxp.json"),
    },
    EmbeddedEntry {
        id: "blyx",
        game_code: "BLYX",
        json: include_str!("../data/blyx.json"),
    },
    EmbeddedEntry {
        id: "bm5e",
        game_code: "BM5E",
        json: include_str!("../data/bm5e.json"),
    },
    EmbeddedEntry {
        id: "bm7e",
        game_code: "BM7E",
        json: include_str!("../data/bm7e.json"),
    },
    EmbeddedEntry {
        id: "bmbe",
        game_code: "BMBE",
        json: include_str!("../data/bmbe.json"),
    },
    EmbeddedEntry {
        id: "bmde",
        game_code: "BMDE",
        json: include_str!("../data/bmde.json"),
    },
    EmbeddedEntry {
        id: "bmep",
        game_code: "BMEP",
        json: include_str!("../data/bmep.json"),
    },
    EmbeddedEntry {
        id: "bmfe",
        game_code: "BMFE",
        json: include_str!("../data/bmfe.json"),
    },
    EmbeddedEntry {
        id: "bmge",
        game_code: "BMGE",
        json: include_str!("../data/bmge.json"),
    },
    EmbeddedEntry {
        id: "bmgu",
        game_code: "BMGU",
        json: include_str!("../data/bmgu.json"),
    },
    EmbeddedEntry {
        id: "bmhj",
        game_code: "BMHJ",
        json: include_str!("../data/bmhj.json"),
    },
    EmbeddedEntry {
        id: "bmme",
        game_code: "BMME",
        json: include_str!("../data/bmme.json"),
    },
    EmbeddedEntry {
        id: "bmqe",
        game_code: "BMQE",
        json: include_str!("../data/bmqe.json"),
    },
    EmbeddedEntry {
        id: "bmrj",
        game_code: "BMRJ",
        json: include_str!("../data/bmrj.json"),
    },
    EmbeddedEntry {
        id: "bmte",
        game_code: "BMTE",
        json: include_str!("../data/bmte.json"),
    },
    EmbeddedEntry {
        id: "bmve",
        game_code: "BMVE",
        json: include_str!("../data/bmve.json"),
    },
    EmbeddedEntry {
        id: "bmxe",
        game_code: "BMXE",
        json: include_str!("../data/bmxe.json"),
    },
    EmbeddedEntry {
        id: "bmxp",
        game_code: "BMXP",
        json: include_str!("../data/bmxp.json"),
    },
    EmbeddedEntry {
        id: "bmzp",
        game_code: "BMZP",
        json: include_str!("../data/bmzp.json"),
    },
    EmbeddedEntry {
        id: "bn4j",
        game_code: "BN4J",
        json: include_str!("../data/bn4j.json"),
    },
    EmbeddedEntry {
        id: "bnbe",
        game_code: "BNBE",
        json: include_str!("../data/bnbe.json"),
    },
    EmbeddedEntry {
        id: "bnce",
        game_code: "BNCE",
        json: include_str!("../data/bnce.json"),
    },
    EmbeddedEntry {
        id: "bnjj",
        game_code: "BNJJ",
        json: include_str!("../data/bnjj.json"),
    },
    EmbeddedEntry {
        id: "bnrj",
        game_code: "BNRJ",
        json: include_str!("../data/bnrj.json"),
    },
    EmbeddedEntry {
        id: "bntp",
        game_code: "BNTP",
        json: include_str!("../data/bntp.json"),
    },
    EmbeddedEntry {
        id: "bnvp",
        game_code: "BNVP",
        json: include_str!("../data/bnvp.json"),
    },
    EmbeddedEntry {
        id: "boae",
        game_code: "BOAE",
        json: include_str!("../data/boae.json"),
    },
    EmbeddedEntry {
        id: "boce",
        game_code: "BOCE",
        json: include_str!("../data/boce.json"),
    },
    EmbeddedEntry {
        id: "bp3j",
        game_code: "BP3J",
        json: include_str!("../data/bp3j.json"),
    },
    EmbeddedEntry {
        id: "bp6j",
        game_code: "BP6J",
        json: include_str!("../data/bp6j.json"),
    },
    EmbeddedEntry {
        id: "bp7j",
        game_code: "BP7J",
        json: include_str!("../data/bp7j.json"),
    },
    EmbeddedEntry {
        id: "bpap",
        game_code: "BPAP",
        json: include_str!("../data/bpap.json"),
    },
    EmbeddedEntry {
        id: "bpbj",
        game_code: "BPBJ",
        json: include_str!("../data/bpbj.json"),
    },
    EmbeddedEntry {
        id: "bpce",
        game_code: "BPCE",
        json: include_str!("../data/bpce.json"),
    },
    EmbeddedEntry {
        id: "bpee",
        game_code: "BPEE",
        json: include_str!("../data/bpee.json"),
    },
    EmbeddedEntry {
        id: "bpfj",
        game_code: "BPFJ",
        json: include_str!("../data/bpfj.json"),
    },
    EmbeddedEntry {
        id: "bpge",
        game_code: "BPGE",
        json: include_str!("../data/bpge.json"),
    },
    EmbeddedEntry {
        id: "bphe",
        game_code: "BPHE",
        json: include_str!("../data/bphe.json"),
    },
    EmbeddedEntry {
        id: "bpkp",
        game_code: "BPKP",
        json: include_str!("../data/bpkp.json"),
    },
    EmbeddedEntry {
        id: "bpmp",
        game_code: "BPMP",
        json: include_str!("../data/bpmp.json"),
    },
    EmbeddedEntry {
        id: "bpnj",
        game_code: "BPNJ",
        json: include_str!("../data/bpnj.json"),
    },
    EmbeddedEntry {
        id: "bppe",
        game_code: "BPPE",
        json: include_str!("../data/bppe.json"),
    },
    EmbeddedEntry {
        id: "bpre",
        game_code: "BPRE",
        json: include_str!("../data/bpre.json"),
    },
    EmbeddedEntry {
        id: "bpue",
        game_code: "BPUE",
        json: include_str!("../data/bpue.json"),
    },
    EmbeddedEntry {
        id: "bpye",
        game_code: "BPYE",
        json: include_str!("../data/bpye.json"),
    },
    EmbeddedEntry {
        id: "bpzj",
        game_code: "BPZJ",
        json: include_str!("../data/bpzj.json"),
    },
    EmbeddedEntry {
        id: "bq3e",
        game_code: "BQ3E",
        json: include_str!("../data/bq3e.json"),
    },
    EmbeddedEntry {
        id: "bq4p",
        game_code: "BQ4P",
        json: include_str!("../data/bq4p.json"),
    },
    EmbeddedEntry {
        id: "bq7p",
        game_code: "BQ7P",
        json: include_str!("../data/bq7p.json"),
    },
    EmbeddedEntry {
        id: "bq9q",
        game_code: "BQ9Q",
        json: include_str!("../data/bq9q.json"),
    },
    EmbeddedEntry {
        id: "bqce",
        game_code: "BQCE",
        json: include_str!("../data/bqce.json"),
    },
    EmbeddedEntry {
        id: "bqnp",
        game_code: "BQNP",
        json: include_str!("../data/bqnp.json"),
    },
    EmbeddedEntry {
        id: "bqtp",
        game_code: "BQTP",
        json: include_str!("../data/bqtp.json"),
    },
    EmbeddedEntry {
        id: "bqvp",
        game_code: "BQVP",
        json: include_str!("../data/bqvp.json"),
    },
    EmbeddedEntry {
        id: "bqyd",
        game_code: "BQYD",
        json: include_str!("../data/bqyd.json"),
    },
    EmbeddedEntry {
        id: "br2e",
        game_code: "BR2E",
        json: include_str!("../data/br2e.json"),
    },
    EmbeddedEntry {
        id: "br5e",
        game_code: "BR5E",
        json: include_str!("../data/br5e.json"),
    },
    EmbeddedEntry {
        id: "br6e",
        game_code: "BR6E",
        json: include_str!("../data/br6e.json"),
    },
    EmbeddedEntry {
        id: "br9j",
        game_code: "BR9J",
        json: include_str!("../data/br9j.json"),
    },
    EmbeddedEntry {
        id: "brap",
        game_code: "BRAP",
        json: include_str!("../data/brap.json"),
    },
    EmbeddedEntry {
        id: "brbe",
        game_code: "BRBE",
        json: include_str!("../data/brbe.json"),
    },
    EmbeddedEntry {
        id: "brhp",
        game_code: "BRHP",
        json: include_str!("../data/brhp.json"),
    },
    EmbeddedEntry {
        id: "brke",
        game_code: "BRKE",
        json: include_str!("../data/brke.json"),
    },
    EmbeddedEntry {
        id: "brle",
        game_code: "BRLE",
        json: include_str!("../data/brle.json"),
    },
    EmbeddedEntry {
        id: "brre",
        game_code: "BRRE",
        json: include_str!("../data/brre.json"),
    },
    EmbeddedEntry {
        id: "brtp",
        game_code: "BRTP",
        json: include_str!("../data/brtp.json"),
    },
    EmbeddedEntry {
        id: "bryp",
        game_code: "BRYP",
        json: include_str!("../data/bryp.json"),
    },
    EmbeddedEntry {
        id: "bs4j",
        game_code: "BS4J",
        json: include_str!("../data/bs4j.json"),
    },
    EmbeddedEntry {
        id: "bsbe",
        game_code: "BSBE",
        json: include_str!("../data/bsbe.json"),
    },
    EmbeddedEntry {
        id: "bsgj",
        game_code: "BSGJ",
        json: include_str!("../data/bsgj.json"),
    },
    EmbeddedEntry {
        id: "bshj",
        game_code: "BSHJ",
        json: include_str!("../data/bshj.json"),
    },
    EmbeddedEntry {
        id: "bsie",
        game_code: "BSIE",
        json: include_str!("../data/bsie.json"),
    },
    EmbeddedEntry {
        id: "bskj",
        game_code: "BSKJ",
        json: include_str!("../data/bskj.json"),
    },
    EmbeddedEntry {
        id: "bslp",
        game_code: "BSLP",
        json: include_str!("../data/bslp.json"),
    },
    EmbeddedEntry {
        id: "bsmp",
        game_code: "BSMP",
        json: include_str!("../data/bsmp.json"),
    },
    EmbeddedEntry {
        id: "bsop",
        game_code: "BSOP",
        json: include_str!("../data/bsop.json"),
    },
    EmbeddedEntry {
        id: "bspe",
        game_code: "BSPE",
        json: include_str!("../data/bspe.json"),
    },
    EmbeddedEntry {
        id: "bste",
        game_code: "BSTE",
        json: include_str!("../data/bste.json"),
    },
    EmbeddedEntry {
        id: "bsue",
        game_code: "BSUE",
        json: include_str!("../data/bsue.json"),
    },
    EmbeddedEntry {
        id: "bsyj",
        game_code: "BSYJ",
        json: include_str!("../data/bsyj.json"),
    },
    EmbeddedEntry {
        id: "bt2p",
        game_code: "BT2P",
        json: include_str!("../data/bt2p.json"),
    },
    EmbeddedEntry {
        id: "bt6p",
        game_code: "BT6P",
        json: include_str!("../data/bt6p.json"),
    },
    EmbeddedEntry {
        id: "bt8e",
        game_code: "BT8E",
        json: include_str!("../data/bt8e.json"),
    },
    EmbeddedEntry {
        id: "bt9x",
        game_code: "BT9X",
        json: include_str!("../data/bt9x.json"),
    },
    EmbeddedEntry {
        id: "btdj",
        game_code: "BTDJ",
        json: include_str!("../data/btdj.json"),
    },
    EmbeddedEntry {
        id: "btee",
        game_code: "BTEE",
        json: include_str!("../data/btee.json"),
    },
    EmbeddedEntry {
        id: "btgp",
        game_code: "BTGP",
        json: include_str!("../data/btgp.json"),
    },
    EmbeddedEntry {
        id: "btij",
        game_code: "BTIJ",
        json: include_str!("../data/btij.json"),
    },
    EmbeddedEntry {
        id: "btlj",
        game_code: "BTLJ",
        json: include_str!("../data/btlj.json"),
    },
    EmbeddedEntry {
        id: "btme",
        game_code: "BTME",
        json: include_str!("../data/btme.json"),
    },
    EmbeddedEntry {
        id: "btnp",
        game_code: "BTNP",
        json: include_str!("../data/btnp.json"),
    },
    EmbeddedEntry {
        id: "btoe",
        game_code: "BTOE",
        json: include_str!("../data/btoe.json"),
    },
    EmbeddedEntry {
        id: "btrj",
        game_code: "BTRJ",
        json: include_str!("../data/btrj.json"),
    },
    EmbeddedEntry {
        id: "bttj",
        game_code: "BTTJ",
        json: include_str!("../data/bttj.json"),
    },
    EmbeddedEntry {
        id: "btue",
        game_code: "BTUE",
        json: include_str!("../data/btue.json"),
    },
    EmbeddedEntry {
        id: "btwe",
        game_code: "BTWE",
        json: include_str!("../data/btwe.json"),
    },
    EmbeddedEntry {
        id: "btze",
        game_code: "BTZE",
        json: include_str!("../data/btze.json"),
    },
    EmbeddedEntry {
        id: "bu7p",
        game_code: "BU7P",
        json: include_str!("../data/bu7p.json"),
    },
    EmbeddedEntry {
        id: "buce",
        game_code: "BUCE",
        json: include_str!("../data/buce.json"),
    },
    EmbeddedEntry {
        id: "budj",
        game_code: "BUDJ",
        json: include_str!("../data/budj.json"),
    },
    EmbeddedEntry {
        id: "buee",
        game_code: "BUEE",
        json: include_str!("../data/buee.json"),
    },
    EmbeddedEntry {
        id: "bulp",
        game_code: "BULP",
        json: include_str!("../data/bulp.json"),
    },
    EmbeddedEntry {
        id: "buoe",
        game_code: "BUOE",
        json: include_str!("../data/buoe.json"),
    },
    EmbeddedEntry {
        id: "buse",
        game_code: "BUSE",
        json: include_str!("../data/buse.json"),
    },
    EmbeddedEntry {
        id: "buuj",
        game_code: "BUUJ",
        json: include_str!("../data/buuj.json"),
    },
    EmbeddedEntry {
        id: "buxd",
        game_code: "BUXD",
        json: include_str!("../data/buxd.json"),
    },
    EmbeddedEntry {
        id: "buye",
        game_code: "BUYE",
        json: include_str!("../data/buye.json"),
    },
    EmbeddedEntry {
        id: "bvbj",
        game_code: "BVBJ",
        json: include_str!("../data/bvbj.json"),
    },
    EmbeddedEntry {
        id: "bvdj",
        game_code: "BVDJ",
        json: include_str!("../data/bvdj.json"),
    },
    EmbeddedEntry {
        id: "bvej",
        game_code: "BVEJ",
        json: include_str!("../data/bvej.json"),
    },
    EmbeddedEntry {
        id: "bw3p",
        game_code: "BW3P",
        json: include_str!("../data/bw3p.json"),
    },
    EmbeddedEntry {
        id: "bw4j",
        game_code: "BW4J",
        json: include_str!("../data/bw4j.json"),
    },
    EmbeddedEntry {
        id: "bw5e",
        game_code: "BW5E",
        json: include_str!("../data/bw5e.json"),
    },
    EmbeddedEntry {
        id: "bw6p",
        game_code: "BW6P",
        json: include_str!("../data/bw6p.json"),
    },
    EmbeddedEntry {
        id: "bwdj",
        game_code: "BWDJ",
        json: include_str!("../data/bwdj.json"),
    },
    EmbeddedEntry {
        id: "bwie",
        game_code: "BWIE",
        json: include_str!("../data/bwie.json"),
    },
    EmbeddedEntry {
        id: "bwkj",
        game_code: "BWKJ",
        json: include_str!("../data/bwkj.json"),
    },
    EmbeddedEntry {
        id: "bwve",
        game_code: "BWVE",
        json: include_str!("../data/bwve.json"),
    },
    EmbeddedEntry {
        id: "bwyp",
        game_code: "BWYP",
        json: include_str!("../data/bwyp.json"),
    },
    EmbeddedEntry {
        id: "bx3p",
        game_code: "BX3P",
        json: include_str!("../data/bx3p.json"),
    },
    EmbeddedEntry {
        id: "bxfp",
        game_code: "BXFP",
        json: include_str!("../data/bxfp.json"),
    },
    EmbeddedEntry {
        id: "bxke",
        game_code: "BXKE",
        json: include_str!("../data/bxke.json"),
    },
    EmbeddedEntry {
        id: "bxse",
        game_code: "BXSE",
        json: include_str!("../data/bxse.json"),
    },
    EmbeddedEntry {
        id: "bxux",
        game_code: "BXUX",
        json: include_str!("../data/bxux.json"),
    },
    EmbeddedEntry {
        id: "by2e",
        game_code: "BY2E",
        json: include_str!("../data/by2e.json"),
    },
    EmbeddedEntry {
        id: "by6p",
        game_code: "BY6P",
        json: include_str!("../data/by6p.json"),
    },
    EmbeddedEntry {
        id: "bydp",
        game_code: "BYDP",
        json: include_str!("../data/bydp.json"),
    },
    EmbeddedEntry {
        id: "bygj",
        game_code: "BYGJ",
        json: include_str!("../data/bygj.json"),
    },
    EmbeddedEntry {
        id: "bylp",
        game_code: "BYLP",
        json: include_str!("../data/bylp.json"),
    },
    EmbeddedEntry {
        id: "bypp",
        game_code: "BYPP",
        json: include_str!("../data/bypp.json"),
    },
    EmbeddedEntry {
        id: "byup",
        game_code: "BYUP",
        json: include_str!("../data/byup.json"),
    },
    EmbeddedEntry {
        id: "bywe",
        game_code: "BYWE",
        json: include_str!("../data/bywe.json"),
    },
    EmbeddedEntry {
        id: "byye",
        game_code: "BYYE",
        json: include_str!("../data/byye.json"),
    },
    EmbeddedEntry {
        id: "bz3e",
        game_code: "BZ3E",
        json: include_str!("../data/bz3e.json"),
    },
    EmbeddedEntry {
        id: "bz4e",
        game_code: "BZ4E",
        json: include_str!("../data/bz4e.json"),
    },
    EmbeddedEntry {
        id: "bz5e",
        game_code: "BZ5E",
        json: include_str!("../data/bz5e.json"),
    },
    EmbeddedEntry {
        id: "bz6e",
        game_code: "BZ6E",
        json: include_str!("../data/bz6e.json"),
    },
    EmbeddedEntry {
        id: "bzme",
        game_code: "BZME",
        json: include_str!("../data/bzme.json"),
    },
    EmbeddedEntry {
        id: "bzwj",
        game_code: "BZWJ",
        json: include_str!("../data/bzwj.json"),
    },
    EmbeddedEntry {
        id: "fade",
        game_code: "FADE",
        json: include_str!("../data/fade.json"),
    },
    EmbeddedEntry {
        id: "fbfj",
        game_code: "FBFJ",
        json: include_str!("../data/fbfj.json"),
    },
    EmbeddedEntry {
        id: "fbme",
        game_code: "FBME",
        json: include_str!("../data/fbme.json"),
    },
    EmbeddedEntry {
        id: "fclj",
        game_code: "FCLJ",
        json: include_str!("../data/fclj.json"),
    },
    EmbeddedEntry {
        id: "fddj",
        game_code: "FDDJ",
        json: include_str!("../data/fddj.json"),
    },
    EmbeddedEntry {
        id: "fdke",
        game_code: "FDKE",
        json: include_str!("../data/fdke.json"),
    },
    EmbeddedEntry {
        id: "fdme",
        game_code: "FDME",
        json: include_str!("../data/fdme.json"),
    },
    EmbeddedEntry {
        id: "febe",
        game_code: "FEBE",
        json: include_str!("../data/febe.json"),
    },
    EmbeddedEntry {
        id: "ffmj",
        game_code: "FFMJ",
        json: include_str!("../data/ffmj.json"),
    },
    EmbeddedEntry {
        id: "fggj",
        game_code: "FGGJ",
        json: include_str!("../data/fggj.json"),
    },
    EmbeddedEntry {
        id: "fice",
        game_code: "FICE",
        json: include_str!("../data/fice.json"),
    },
    EmbeddedEntry {
        id: "flbe",
        game_code: "FLBE",
        json: include_str!("../data/flbe.json"),
    },
    EmbeddedEntry {
        id: "fmkj",
        game_code: "FMKJ",
        json: include_str!("../data/fmkj.json"),
    },
    EmbeddedEntry {
        id: "fmpj",
        game_code: "FMPJ",
        json: include_str!("../data/fmpj.json"),
    },
    EmbeddedEntry {
        id: "fmre",
        game_code: "FMRE",
        json: include_str!("../data/fmre.json"),
    },
    EmbeddedEntry {
        id: "fnmj",
        game_code: "FNMJ",
        json: include_str!("../data/fnmj.json"),
    },
    EmbeddedEntry {
        id: "fp7e",
        game_code: "FP7E",
        json: include_str!("../data/fp7e.json"),
    },
    EmbeddedEntry {
        id: "fpmj",
        game_code: "FPMJ",
        json: include_str!("../data/fpmj.json"),
    },
    EmbeddedEntry {
        id: "fptj",
        game_code: "FPTJ",
        json: include_str!("../data/fptj.json"),
    },
    EmbeddedEntry {
        id: "fsdj",
        game_code: "FSDJ",
        json: include_str!("../data/fsdj.json"),
    },
    EmbeddedEntry {
        id: "fsme",
        game_code: "FSME",
        json: include_str!("../data/fsme.json"),
    },
    EmbeddedEntry {
        id: "fsoj",
        game_code: "FSOJ",
        json: include_str!("../data/fsoj.json"),
    },
    EmbeddedEntry {
        id: "fsrj",
        game_code: "FSRJ",
        json: include_str!("../data/fsrj.json"),
    },
    EmbeddedEntry {
        id: "ftbj",
        game_code: "FTBJ",
        json: include_str!("../data/ftbj.json"),
    },
    EmbeddedEntry {
        id: "ftkj",
        game_code: "FTKJ",
        json: include_str!("../data/ftkj.json"),
    },
    EmbeddedEntry {
        id: "ftuj",
        game_code: "FTUJ",
        json: include_str!("../data/ftuj.json"),
    },
    EmbeddedEntry {
        id: "ftwj",
        game_code: "FTWJ",
        json: include_str!("../data/ftwj.json"),
    },
    EmbeddedEntry {
        id: "fwcj",
        game_code: "FWCJ",
        json: include_str!("../data/fwcj.json"),
    },
    EmbeddedEntry {
        id: "fxve",
        game_code: "FXVE",
        json: include_str!("../data/fxve.json"),
    },
    EmbeddedEntry {
        id: "fzlj",
        game_code: "FZLJ",
        json: include_str!("../data/fzlj.json"),
    },
    EmbeddedEntry {
        id: "ghtj",
        game_code: "GHTJ",
        json: include_str!("../data/ghtj.json"),
    },
    EmbeddedEntry {
        id: "metj",
        game_code: "METJ",
        json: include_str!("../data/metj.json"),
    },
    EmbeddedEntry {
        id: "rare",
        game_code: "RARE",
        json: include_str!("../data/rare.json"),
    },
    EmbeddedEntry {
        id: "rzwe",
        game_code: "RZWE",
        json: include_str!("../data/rzwe.json"),
    },
    EmbeddedEntry {
        id: "tchk",
        game_code: "TCHK",
        json: include_str!("../data/tchk.json"),
    },
    EmbeddedEntry {
        id: "zmde",
        game_code: "ZMDE",
        json: include_str!("../data/zmde.json"),
    },
];

/// Return the total count of embedded save format definitions.
pub const fn count() -> usize {
    REGISTRY.len()
}

/// Return all embedded save-definition IDs.
pub fn list_ids() -> Vec<&'static str> {
    REGISTRY.iter().map(|e| e.id).collect()
}

/// Return all embedded 4-character GBA game codes.
pub fn list_game_codes() -> Vec<&'static str> {
    REGISTRY.iter().map(|e| e.game_code).collect()
}

/// Parse and return every embedded [`SaveDefinition`].
pub fn all() -> Result<Vec<SaveDefinition>> {
    REGISTRY
        .iter()
        .map(|e| SaveDefinition::from_json(e.json))
        .collect()
}

/// Look up an embedded [`SaveDefinition`] by `id` or 4-character `game_code`
/// (case-insensitive). If no exact match is found, falls back to a 3-character
/// family prefix match.
pub fn find(id_or_game_code: &str) -> Result<Option<SaveDefinition>> {
    let needle = id_or_game_code.trim();
    for entry in REGISTRY {
        if entry.id.eq_ignore_ascii_case(needle) || entry.game_code.eq_ignore_ascii_case(needle) {
            return SaveDefinition::from_json(entry.json).map(Some);
        }
    }
    if needle.len() >= 3 {
        let prefix = &needle[..3];
        for entry in REGISTRY {
            if entry.game_code.len() >= 3 && entry.game_code[..3].eq_ignore_ascii_case(prefix) {
                return SaveDefinition::from_json(entry.json).map(Some);
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gba_save::{decode_save, fix_checksums, verify_save};

    #[test]
    fn all_embedded_definitions_parse_and_validate() {
        let defs = all().expect("all embedded JSON definitions must parse and validate");
        assert_eq!(defs.len(), count());
        assert_eq!(defs.len(), 846);
    }

    #[test]
    fn lookup_by_game_code_and_id_case_insensitive() {
        let by_upper = find("AHRE").unwrap().expect("found AHRE");
        let by_lower = find("ahre").unwrap().expect("found ahre");
        assert_eq!(by_upper, by_lower);
        let by_family = find("AHR").unwrap().expect("found by family prefix AHR");
        assert_eq!(by_upper, by_family);
        assert!(find("ZZZZ").unwrap().is_none());
    }

    #[test]
    fn synthetic_round_trip_for_every_embedded_definition() {
        for def in all().unwrap() {
            let mut buf = vec![0x33u8; def.size_bytes];
            for sig in &def.magic_signatures {
                buf[sig.offset..sig.offset + sig.bytes.len()].copy_from_slice(&sig.bytes);
            }
            if def.eeprom_rev8 {
                gba_save::reverse_eeprom_blocks_8(&mut buf);
            }
            fix_checksums(&def, &mut buf)
                .unwrap_or_else(|e| panic!("fix_checksums failed for {}: {e}", def.id));
            let report = verify_save(&def, &buf)
                .unwrap_or_else(|e| panic!("verify_save failed for {}: {e}", def.id));
            assert!(
                report.valid,
                "synthetic save for {} failed verification: {:?}",
                def.id, report
            );
            let decoded = decode_save(&def, &buf)
                .unwrap_or_else(|e| panic!("decode_save failed for {}: {e}", def.id));
            assert_eq!(decoded.fields.len(), def.fields.len());
        }
    }
}
