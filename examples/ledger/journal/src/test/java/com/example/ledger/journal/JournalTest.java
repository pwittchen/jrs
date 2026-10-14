package com.example.ledger.journal;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.example.ledger.core.Money;
import java.io.StringReader;
import org.junit.jupiter.api.Test;

class JournalTest {

    @Test
    void rows_sharing_an_id_make_one_transaction() throws Exception {
        var ledger = Journal.read(new StringReader("""
                id,date,description,account,amount,currency
                1,2026-01-01,Salary,assets:bank,2500.00,EUR
                1,2026-01-01,Salary,income:salary,-2500.00,EUR

                2,2026-01-05,Rent,expenses:rent,900.00,EUR
                2,2026-01-05,Rent,assets:bank,-900.00,EUR
                """));
        assertEquals(2, ledger.transactions().size());
        assertEquals(Money.parse("1600", "EUR"), ledger.balances().get("assets").get("EUR"));
    }

    @Test
    void an_unbalanced_transaction_names_its_id() {
        var thrown = assertThrows(Journal.JournalException.class, () -> Journal.read(new StringReader("""
                id,date,description,account,amount,currency
                7,2026-02-01,Typo,expenses:food,10.00,EUR
                7,2026-02-01,Typo,assets:cash,-1.00,EUR
                """)));
        assertTrue(thrown.getMessage().startsWith("transaction 7 (line 1): "));
    }
}
