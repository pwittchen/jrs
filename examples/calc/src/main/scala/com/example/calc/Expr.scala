package com.example.calc

/** An arithmetic expression. */
enum Expr:
  case Num(value: Double)
  case Neg(expr: Expr)
  case Add(left: Expr, right: Expr)
  case Sub(left: Expr, right: Expr)
  case Mul(left: Expr, right: Expr)
  case Div(left: Expr, right: Expr)

object Expr:
  extension (e: Expr)
    def eval: Double = e match
      case Num(v)    => v
      case Neg(x)    => -x.eval
      case Add(l, r) => l.eval + r.eval
      case Sub(l, r) => l.eval - r.eval
      case Mul(l, r) => l.eval * r.eval
      case Div(l, r) =>
        val divisor = r.eval
        if divisor == 0 then throw ArithmeticException("division by zero")
        else l.eval / divisor
