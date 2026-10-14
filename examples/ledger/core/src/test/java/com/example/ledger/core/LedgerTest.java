package com.example.ledger.core;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.time.LocalDate;
import java.util.List;
import org.junit.jupiter.api.Test;

class LedgerTest {

    private static Posting posting(String account, String amount) {
        return new Posting(account, Money.parse(amount, "EUR"));
    }

    @Test
    void money_parses_and_prints_exactly() {
        assertEquals(-1250, Money.parse("-12.5", "EUR").cents());
        assertEquals("0.10 EUR", Money.parse("0.1", "EUR").toString());
        assertThrows(IllegalArgumentException.class, () -> Money.parse("1.005", "EUR"));
    }

    @Test
    void a_transaction_must_balance() {
        var thrown = assertThrows(
                Transaction.UnbalancedException.class,
                () -> new Transaction(LocalDate.of(2026, 1, 2), "Lunch",
                        List.of(posting("expenses:food", "12.00"), posting("assets:cash", "-10.00"))));
        assertEquals("\"Lunch\" does not balance: it is off by 2.00 EUR", thrown.getMessage());
    }

    @Test
    void balances_roll_up_into_parent_accounts() {
        var ledger = new Ledger()
                .record(new Transaction(LocalDate.of(2026, 1, 1), "Salary",
                        List.of(posting("assets:bank", "2500.00"), posting("income:salary", "-2500.00"))))
                .record(new Transaction(LocalDate.of(2026, 1, 2), "Cash",
                        List.of(posting("assets:cash", "100.00"), posting("assets:bank", "-100.00"))));
        var balances = ledger.balances();
        assertEquals(Money.parse("2400", "EUR"), balances.get("assets:bank").get("EUR"));
        assertEquals(Money.parse("2500", "EUR"), balances.get("assets").get("EUR"));
        assertEquals(Money.parse("-2500", "EUR"), balances.get("income").get("EUR"));
    }
}
