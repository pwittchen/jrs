package com.example.ledger.app;

import static org.junit.jupiter.api.Assertions.assertEquals;

import com.example.ledger.core.Ledger;
import com.example.ledger.core.Money;
import com.example.ledger.core.Posting;
import com.example.ledger.core.Transaction;
import java.time.LocalDate;
import java.util.List;
import org.junit.jupiter.api.Test;

class ReportTest {

    @Test
    void accounts_are_indented_under_their_parents() {
        var ledger = new Ledger().record(new Transaction(LocalDate.of(2026, 1, 1), "Salary", List.of(
                new Posting("assets:bank", Money.parse("10", "EUR")),
                new Posting("income:salary", Money.parse("-10", "EUR")))));
        assertEquals(String.join(System.lineSeparator(),
                "assets                        10.00 EUR",
                "  bank                        10.00 EUR",
                "income                       -10.00 EUR",
                "  salary                     -10.00 EUR",
                ""), Report.render(ledger));
    }
}
