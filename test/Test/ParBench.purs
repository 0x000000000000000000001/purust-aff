module Test.ParBench where

import Prelude

import Control.Monad.Rec.Class (Step(..), tailRecM)
import Control.Parallel (parTraverse)
import Data.Array as Array
import Effect (Effect)
import Effect.Aff (Aff, launchAff_)
import Effect.Class (liftEffect)
import Effect.Console as Console

-- | A CPU-bound `Aff` action: no suspension, a fixed number of binds. Used to
-- | measure how `parTraverse` scales with `PURUST_AFF_WORKERS`.
burn :: Int -> Aff Int
burn = tailRecM \n ->
  if n <= 0 then pure (Done 0)
  else pure (Loop (n - 1))

tasks :: Int
tasks = 8

steps :: Int
steps = 500000

main :: Effect Unit
main = launchAff_ do
  results <- parTraverse burn (Array.replicate tasks steps)
  liftEffect $ Console.log $ "sum: " <> show (Array.foldl (+) 0 results)
