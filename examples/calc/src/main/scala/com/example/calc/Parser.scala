package com.example.calc

import Expr.*

/** Parses `1 + 2 * (3 - 4)`, with the usual precedence, left to right. */
final class Parser private (text: String):
  private var pos = 0

  private def peek: Option[Char] =
    while pos < text.length && text(pos).isWhitespace do pos += 1
    text.lift(pos)

  private def fail(what: String): Nothing =
    throw IllegalArgumentException(s"$what at ${pos + 1} in '$text'")

  private def expr(): Expr =
    var left = term()
    var more = true
    while more do
      peek match
        case Some('+') => pos += 1; left = Add(left, term())
        case Some('-') => pos += 1; left = Sub(left, term())
        case _         => more = false
    left

  private def term(): Expr =
    var left = factor()
    var more = true
    while more do
      peek match
        case Some('*') => pos += 1; left = Mul(left, factor())
        case Some('/') => pos += 1; left = Div(left, factor())
        case _         => more = false
    left

  private def factor(): Expr =
    peek match
      case Some('-') => pos += 1; Neg(factor())
      case Some('(') =>
        pos += 1
        val inner = expr()
        if peek.contains(')') then pos += 1 else fail("expected ')'")
        inner
      case Some(c) if c.isDigit || c == '.' =>
        val start = pos
        while pos < text.length && (text(pos).isDigit || text(pos) == '.') do pos += 1
        text.substring(start, pos).toDoubleOption.map(Num(_)).getOrElse(fail("not a number"))
      case Some(c) => fail(s"unexpected '$c'")
      case None    => fail("unexpected end")

  private def whole(): Expr =
    val e = expr()
    peek.foreach(c => fail(s"unexpected '$c'"))
    e

object Parser:
  def parse(text: String): Expr = Parser(text).whole()
